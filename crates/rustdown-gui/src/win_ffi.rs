//! The Win32 calls behind file association, single instance, the
//! frameless window's DWM styling and its icon.
//!
//! Every `unsafe` block of the Windows integration lives here (besides
//! `cli::attach_parent_console`); each wrapper is a thin function with
//! `SAFETY` notes and a safe signature.  Registry access goes through the
//! safe `winreg` crate instead.

use std::{
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    time::Duration,
};

use windows_sys::Win32::{
    Foundation::{ERROR_PIPE_CONNECTED, FILETIME, GlobalFree, HWND, INVALID_HANDLE_VALUE, POINT},
    Graphics::Dwm::{
        DWM_WINDOW_CORNER_PREFERENCE, DWMWA_USE_IMMERSIVE_DARK_MODE,
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
        DwmSetWindowAttribute,
    },
    Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint},
    Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
            OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
        },
        LibraryLoader::GetModuleHandleW,
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        Ole::{CF_DIB, CF_UNICODETEXT},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        },
        RemoteDesktop::ProcessIdToSessionId,
        SystemInformation::GetSystemTimePreciseAsFileTime,
        Threading::{GetCurrentProcess, GetProcessTimes},
    },
    UI::{
        Controls::MARGINS,
        Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_V},
        Shell::{
            ASSOCF_INIT_IGNOREUNKNOWN, ASSOCSTR_EXECUTABLE, ASSOCSTR_FRIENDLYAPPNAME,
            AssocQueryStringW, SHARD_PATHW, SHAddToRecentDocs, SHCNE_ASSOCCHANGED, SHCNF_IDLIST,
            SHChangeNotify, ShellExecuteW,
        },
        WindowsAndMessaging::{
            AllowSetForegroundWindow, GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, IsIconic,
            LR_DEFAULTCOLOR, LoadImageW, MB_ICONERROR, MB_OK, MessageBoxW, SM_CXICON, SM_CXSMICON,
            SM_CYICON, SM_CYSMICON, SPI_GETCLIENTAREAANIMATION, SW_RESTORE, SW_SHOWNORMAL,
            SendMessageW, ShowWindowAsync, SystemParametersInfoW, WM_SETICON,
        },
    },
};

/// In/out buffer size of each pipe instance; requests are usually far smaller.
const PIPE_BUFFER_SIZE: u32 = 4096;

/// `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Tell the shell that file associations changed (refreshes icons and
/// "Open with").
#[allow(unsafe_code)]
pub(crate) fn notify_associations_changed() {
    // SAFETY: `SHCNE_ASSOCCHANGED` takes no items, so both item pointers are
    // null as documented; the call has no other preconditions.
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED.cast_signed(),
            SHCNF_IDLIST,
            ptr::null(),
            ptr::null(),
        );
    }
}

/// What [`file_type_handler`] asks about the default app of a file type.
#[derive(Clone, Copy, Debug)]
pub(crate) enum HandlerInfo {
    /// Its exe's path.
    Executable,
    /// Its display name.
    FriendlyName,
}

/// The default app for files ending in `ext` (e.g. `.md`), resolved as
/// Explorer resolves a double-click (the user's choice included); `None`
/// when no app is set.
#[allow(unsafe_code)]
pub(crate) fn file_type_handler(ext: &str, info: HandlerInfo) -> Option<String> {
    let what = match info {
        HandlerInfo::Executable => ASSOCSTR_EXECUTABLE,
        HandlerInfo::FriendlyName => ASSOCSTR_FRIENDLYAPPNAME,
    };
    let ext = wide(ext);
    let verb = wide("open");
    let mut buffer = [0_u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: `ext` and `verb` are NUL-terminated UTF-16 buffers that
    // outlive the call; `buffer` is writable for `len` UTF-16 units, and
    // `len` is a live `u32` the call updates.  With "ignore unknown" an
    // extension without an app fails instead of naming "Open with".
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_IGNOREUNKNOWN,
            what,
            ext.as_ptr(),
            verb.as_ptr(),
            buffer.as_mut_ptr(),
            &raw mut len,
        )
    };
    if result < 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..end]);
    (!text.is_empty()).then_some(text)
}

/// Open `target` (e.g. an `ms-settings:` URI) with its registered handler.
#[allow(unsafe_code)]
pub(crate) fn shell_open(target: &str) -> io::Result<()> {
    let verb = wide("open");
    let file = wide(target);
    // SAFETY: `verb` and `file` are NUL-terminated UTF-16 buffers that outlive
    // the call; a null owner window, parameters and directory are allowed.
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value greater than 32 on success.
    if result.addr() > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "could not open {target} (error {})",
            result.addr()
        )))
    }
}

/// Remote Desktop / fast-user-switching session of this process.
#[allow(unsafe_code)]
pub(crate) fn current_session_id() -> Option<u32> {
    let mut session = 0;
    // SAFETY: `session` is a live, writable `u32` for the whole call.
    let ok = unsafe { ProcessIdToSessionId(std::process::id(), &raw mut session) };
    (ok != 0).then_some(session)
}

/// Create one server instance of the named pipe `name` (`\\.\pipe\…`),
/// local clients only.  With `first`, creation fails with
/// `ERROR_ACCESS_DENIED` when the pipe already exists — an atomic
/// "am I the first instance?" test.
#[allow(unsafe_code)]
pub(crate) fn create_pipe_instance(name: &str, first: bool) -> io::Result<File> {
    let name = wide(name);
    let open_mode = if first {
        PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        PIPE_ACCESS_DUPLEX
    };
    let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the
    // call; null security attributes select the default DACL (full control
    // for this user, SYSTEM and administrators; read-only for others, so
    // other users cannot send requests).
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            pipe_mode,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_SIZE,
            PIPE_BUFFER_SIZE,
            0,
            ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is a valid pipe handle that nothing else owns; the
    // `File` closes it exactly once on drop.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
}

/// Block until a client connects to the server instance `pipe`.
#[allow(unsafe_code)]
pub(crate) fn wait_for_client(pipe: &File) -> io::Result<()> {
    // SAFETY: `pipe` is a live server-instance handle opened for synchronous
    // I/O, for which a null OVERLAPPED pointer is the documented usage.
    let ok = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), ptr::null_mut()) };
    if ok != 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // A client that connected before this call is reported as an error.
    if err.raw_os_error() == Some(ERROR_PIPE_CONNECTED.cast_signed()) {
        Ok(())
    } else {
        Err(err)
    }
}

/// Process id of the server end of the client pipe `pipe`.
#[allow(unsafe_code)]
pub(crate) fn pipe_server_process_id(pipe: &File) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: `pipe` is a live client pipe handle and `pid` a writable `u32`
    // for the whole call.
    let ok = unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &raw mut pid) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(pid)
    }
}

/// Let process `pid` bring its window to the front.  Only works while this
/// process may set the foreground window itself (e.g. it was just started
/// by Explorer); otherwise it is a harmless no-op.
#[allow(unsafe_code)]
pub(crate) fn allow_set_foreground_window(pid: u32) {
    // SAFETY: plain value argument, no pointers.
    let _ = unsafe { AllowSetForegroundWindow(pid) };
}

/// Restore the window `hwnd` (an `HWND` value) if it is minimized.  Never
/// blocks, even when the window's thread is busy.
#[allow(unsafe_code)]
pub(crate) fn restore_if_minimized(hwnd: isize) {
    let hwnd = hwnd as HWND;
    // SAFETY: both functions accept any HWND value — a stale or invalid one
    // only makes them fail.  `ShowWindowAsync` posts the request to the
    // window's own thread instead of waiting for it.
    unsafe {
        if IsIconic(hwnd) != 0 {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
    }
}

/// Ask DWM to round the corners of the window `hwnd` (an `HWND` value), as
/// Windows 11 does for windows with a standard frame.  Before Windows 11 the
/// attribute is unknown and the call fails harmlessly.
#[allow(unsafe_code)]
pub(crate) fn round_window_corners(hwnd: isize) {
    let preference: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
    // SAFETY: `pvattribute` points to `preference`, a live `i32` for the whole
    // call, and `cbattribute` is its size, as the attribute requires.  A stale
    // or invalid HWND only makes the call return an error.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd as HWND,
            DWMWA_WINDOW_CORNER_PREFERENCE.cast_unsigned(),
            (&raw const preference).cast(),
            size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        )
    };
}

/// Give the frameless window `hwnd` (an `HWND` value) the standard DWM drop
/// shadow by extending the window frame 1 px into the client area (which
/// the app paints over, opaquely).
#[allow(unsafe_code)]
pub(crate) fn enable_frame_shadow(hwnd: isize) {
    let margins = MARGINS {
        cxLeftWidth: 1,
        cxRightWidth: 1,
        cyTopHeight: 1,
        cyBottomHeight: 1,
    };
    // SAFETY: `margins` is a live `MARGINS` for the whole call and is only
    // read.  A stale or invalid HWND only makes the call return an error.
    let _ = unsafe { DwmExtendFrameIntoClientArea(hwnd as HWND, &raw const margins) };
}

/// Tell DWM whether the window `hwnd` (an `HWND` value) is dark: the
/// backdrop, the window border and the frame use their dark variants.
/// Before Windows 10 20H1 the attribute is unknown and the call fails
/// harmlessly.
#[allow(unsafe_code)]
pub(crate) fn set_dark_frame(hwnd: isize, dark: bool) {
    let value: windows_sys::core::BOOL = dark.into();
    // SAFETY: `pvattribute` points to `value`, a live `BOOL` for the whole
    // call, and `cbattribute` is its size, as the attribute requires.  A stale
    // or invalid HWND only makes the call return an error.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd as HWND,
            DWMWA_USE_IMMERSIVE_DARK_MODE.cast_unsigned(),
            (&raw const value).cast(),
            size_of::<windows_sys::core::BOOL>() as u32,
        )
    };
}

/// Whether Windows' "Animation effects" setting is on (Settings,
/// Accessibility, Visual effects); `true` when Windows does not say.
#[allow(unsafe_code)]
pub(crate) fn animation_effects_enabled() -> bool {
    let mut enabled: windows_sys::core::BOOL = 1;
    // SAFETY: for SPI_GETCLIENTAREAANIMATION `pvparam` points to a `BOOL`
    // that receives the setting; `enabled` is live and writable for the
    // whole call.  Nothing is written to the user profile (flags 0).
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0)
    };
    ok == 0 || enabled != 0
}

/// Whether the point (`x`, `y`) is on one of the monitors.  Called before
/// the window exists, when the process is not yet DPI aware, so Windows
/// takes logical pixels, as the settings file keeps them.
#[allow(unsafe_code)]
pub(crate) fn point_on_a_monitor(x: f32, y: f32) -> bool {
    let point = POINT {
        x: x.round() as i32,
        y: y.round() as i32,
    };
    // SAFETY: plain value arguments; the call only looks the point up.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) };
    !monitor.is_null()
}

/// An image on the clipboard: a PNG file's bytes (apps that copy images
/// usually offer one), else a device-independent bitmap (`CF_DIB`).
pub(crate) enum ClipboardImage {
    Png(Vec<u8>),
    Dib(Vec<u8>),
}

/// The clipboard, open until dropped.
struct OpenedClipboard;

impl OpenedClipboard {
    #[allow(unsafe_code)]
    fn open() -> Option<Self> {
        // SAFETY: a null owner window is allowed; it is closed on drop.
        (unsafe { OpenClipboard(ptr::null_mut()) } != 0).then_some(Self)
    }
}

impl Drop for OpenedClipboard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: this value exists only while the clipboard is open.
        let _ = unsafe { CloseClipboard() };
    }
}

/// The id of the clipboard format named `name` (e.g. "PNG").
#[allow(unsafe_code)]
fn clipboard_format(name: &str) -> u32 {
    let name = wide(name);
    // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the call.
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// A copy of the clipboard data in `format`, if it has some (the clipboard
/// must be open).
#[allow(unsafe_code)]
fn clipboard_bytes(format: u32) -> Option<Vec<u8>> {
    // SAFETY: plain value arguments; the clipboard is open (caller).
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return None;
    }
    // SAFETY: `handle` is the clipboard's global memory; it is locked only
    // while copied and stays owned by the clipboard.
    unsafe {
        let size = GlobalSize(handle);
        let data = GlobalLock(handle).cast::<u8>();
        if data.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(data, size).to_vec();
        let _ = GlobalUnlock(handle);
        Some(bytes)
    }
}

/// The image on the clipboard, if there is one (and no text: text pastes
/// as text).
#[allow(unsafe_code)]
pub(crate) fn clipboard_image() -> Option<ClipboardImage> {
    let _open = OpenedClipboard::open()?;
    let png = clipboard_format("PNG");
    // SAFETY: plain value arguments.
    let (has_png, has_dib) = unsafe {
        (
            IsClipboardFormatAvailable(png) != 0,
            IsClipboardFormatAvailable(u32::from(CF_DIB)) != 0,
        )
    };
    if has_png && let Some(bytes) = clipboard_bytes(png) {
        return Some(ClipboardImage::Png(bytes));
    }
    if has_dib {
        return clipboard_bytes(u32::from(CF_DIB)).map(ClipboardImage::Dib);
    }
    None
}

/// Copy `bytes` into new movable global memory for the clipboard.
#[allow(unsafe_code)]
fn global_copy(bytes: &[u8]) -> Option<*mut core::ffi::c_void> {
    // SAFETY: plain value arguments; the memory is freed by the caller if
    // the clipboard does not take it.
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if handle.is_null() {
            return None;
        }
        let data = GlobalLock(handle).cast::<u8>();
        if data.is_null() {
            let _ = GlobalFree(handle);
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        let _ = GlobalUnlock(handle);
        Some(handle)
    }
}

/// Put `html` (in "HTML Format", see `md::clipboard_html`) and `text` on
/// the clipboard together: apps that take formatting paste the HTML, the
/// others the text.
#[allow(unsafe_code)]
pub(crate) fn set_clipboard_html_and_text(html: &str, text: &str) -> io::Result<()> {
    let Some(_open) = OpenedClipboard::open() else {
        return Err(io::Error::last_os_error());
    };
    // SAFETY: the clipboard is open.
    if unsafe { EmptyClipboard() } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut html_bytes = html.as_bytes().to_vec();
    html_bytes.push(0);
    let text_bytes: Vec<u8> = text
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    for (format, bytes) in [
        (clipboard_format("HTML Format"), html_bytes),
        (u32::from(CF_UNICODETEXT), text_bytes),
    ] {
        let Some(handle) = global_copy(&bytes) else {
            return Err(io::Error::last_os_error());
        };
        // SAFETY: the clipboard is open and `handle` is movable global
        // memory; on success the clipboard owns it, else it is freed here.
        unsafe {
            if SetClipboardData(format, handle).is_null() {
                let _ = GlobalFree(handle);
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Whether Ctrl and V are held down right now.
#[allow(unsafe_code)]
pub(crate) fn ctrl_v_down() -> bool {
    // SAFETY: plain value arguments.  The top bit is set while a key is
    // down, which makes the `i16` negative.
    unsafe { GetAsyncKeyState(i32::from(VK_CONTROL)) < 0 && GetAsyncKeyState(i32::from(VK_V)) < 0 }
}

/// Add `path` to Windows' recent documents (the taskbar icon's Jump List).
#[allow(unsafe_code)]
pub(crate) fn add_to_recent_documents(path: &std::path::Path) {
    let path = wide(&path.to_string_lossy());
    // SAFETY: `path` is a NUL-terminated UTF-16 buffer that outlives the
    // call, as SHARD_PATHW requires.
    unsafe { SHAddToRecentDocs(SHARD_PATHW.cast_unsigned(), path.as_ptr().cast()) };
}

/// How long ago this process was created, by the system clock: it includes
/// the time Windows took to load the exe before `main` ran.
#[allow(unsafe_code)]
pub(crate) fn process_age() -> Option<Duration> {
    let mut created = FILETIME::default();
    let (mut exited, mut kernel, mut user, mut now) = (created, created, created, created);
    // SAFETY: the pseudo handle of the current process needs no release, and
    // every out pointer is a live, writable `FILETIME` for the whole call.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if ok == 0 {
        return None;
    }
    // SAFETY: `now` is a live, writable `FILETIME`.
    unsafe { GetSystemTimePreciseAsFileTime(&raw mut now) };
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    // FILETIME counts 100 ns ticks.
    let age = ticks(now).checked_sub(ticks(created))?;
    Some(Duration::from_nanos(age.saturating_mul(100)))
}

/// Give the window `hwnd` (an `HWND` value) the exe's icon (resource 1,
/// `windows/mdnotepad.ico`) for the taskbar / Alt+Tab (`ICON_BIG`) and the
/// small icon (`ICON_SMALL`), each loaded at the size Windows uses for it,
/// so the .ico's hand-made 16-64 px images are picked, not a downscale.
/// Without the resource (a build without a resource compiler) nothing
/// changes.
#[allow(unsafe_code)]
pub(crate) fn set_window_icons_from_exe(hwnd: isize) {
    /// The icon's resource id in `windows/mdnotepad.rc`.
    const APP_ICON_ID: usize = 1;
    // SAFETY: a null name asks for this exe's own module handle, which
    // needs no release.
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    for (kind, width, height) in [
        (ICON_BIG, SM_CXICON, SM_CYICON),
        (ICON_SMALL, SM_CXSMICON, SM_CYSMICON),
    ] {
        // SAFETY: plain value arguments.
        let (cx, cy) = unsafe { (GetSystemMetrics(width), GetSystemMetrics(height)) };
        // SAFETY: `module` is this exe and the name is an integer resource
        // id (MAKEINTRESOURCE), which LoadImageW accepts in place of a
        // string; a missing resource returns null.  The icon stays loaded
        // for the life of the window, which owns no copy of it.
        let icon = unsafe {
            LoadImageW(
                module,
                ptr::without_provenance(APP_ICON_ID),
                IMAGE_ICON,
                cx,
                cy,
                LR_DEFAULTCOLOR,
            )
        };
        if !icon.is_null() {
            // SAFETY: WM_SETICON takes the icon handle by value; a stale or
            // invalid HWND only makes the call fail.
            let _ = unsafe { SendMessageW(hwnd as HWND, WM_SETICON, kind as usize, icon as isize) };
        }
    }
}

/// Show a modal error message box with no owner window (crash reports).
#[allow(unsafe_code)]
pub(crate) fn error_box(title: &str, text: &str) {
    let title = wide(title);
    let text = wide(text);
    // SAFETY: both buffers are NUL-terminated UTF-16 that outlive the call;
    // a null owner window is allowed.
    let _ = unsafe {
        MessageBoxW(
            ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}
