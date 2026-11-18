//! The COM side of the preview handler, written by hand (no COM crate):
//! the class factory Explorer gets from [`DllGetClassObject`], and the
//! handler it makes, with the interfaces a preview handler has —
//! `IPreviewHandler`, `IInitializeWithStream` / `IInitializeWithFile`,
//! `IObjectWithSite` and `IOleWindow`.  The handler reads the file when
//! initialised and shows it in a read-only rich-text control (`RichEdit`,
//! `Msftedit.dll`) on the window Explorer gives it.
//!
//! The whole module is FFI, so `unsafe` is allowed here (and only here in
//! the DLL); each block says why it is sound.  Explorer calls a handler on
//! one thread (an apartment-threaded object), so its state is a `RefCell`.

#![allow(unsafe_code)]
#![allow(clippy::missing_const_for_fn)] // COM entry points.

use std::{
    cell::RefCell,
    ffi::c_void,
    mem::offset_of,
    os::windows::ffi::OsStringExt,
    ptr,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

/// A COM GUID.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

impl Guid {
    const fn from_u128(value: u128) -> Self {
        Self {
            data1: (value >> 96) as u32,
            data2: (value >> 80) as u16,
            data3: (value >> 64) as u16,
            data4: (value as u64).to_be_bytes(),
        }
    }
}

/// The handler's class id (also in the app's registration,
/// `win_file_assoc::PREVIEW_CLSID`).
const CLSID_PREVIEW: Guid = Guid::from_u128(0x5f3b_6c2e_8d4a_4b7f_9e21_7c6a_1d93_b0e4);
const IID_UNKNOWN: Guid = Guid::from_u128(0x0000_0000_0000_0000_c000_0000_0000_0046);
const IID_CLASS_FACTORY: Guid = Guid::from_u128(0x0000_0001_0000_0000_c000_0000_0000_0046);
const IID_PREVIEW_HANDLER: Guid = Guid::from_u128(0x8895_b1c6_b41f_4c1c_a562_0d56_4250_836f);
const IID_INITIALIZE_WITH_STREAM: Guid = Guid::from_u128(0xb824_b49d_22ac_4161_ac8a_9916_e8fa_3f7f);
const IID_INITIALIZE_WITH_FILE: Guid = Guid::from_u128(0xb7d1_4566_0509_4cce_a71f_0a55_4233_bd9b);
const IID_OBJECT_WITH_SITE: Guid = Guid::from_u128(0xfc48_01a3_2ba9_11cf_a229_00aa_003d_7352);
const IID_OLE_WINDOW: Guid = Guid::from_u128(0x0000_0114_0000_0000_c000_0000_0000_0046);

type Hresult = i32;
type Hwnd = *mut c_void;
type This = *mut c_void;

const S_OK: Hresult = 0;
const S_FALSE: Hresult = 1;
const E_NOTIMPL: Hresult = 0x8000_4001_u32.cast_signed();
const E_NOINTERFACE: Hresult = 0x8000_4002_u32.cast_signed();
const E_POINTER: Hresult = 0x8000_4003_u32.cast_signed();
const E_FAIL: Hresult = 0x8000_4005_u32.cast_signed();
const E_UNEXPECTED: Hresult = 0x8000_ffff_u32.cast_signed();
const CLASS_E_NOAGGREGATION: Hresult = 0x8004_0110_u32.cast_signed();
const CLASS_E_CLASSNOTAVAILABLE: Hresult = 0x8004_0111_u32.cast_signed();

const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const ES_MULTILINE: u32 = 0x0004;
const ES_AUTOVSCROLL: u32 = 0x0040;
const ES_READONLY: u32 = 0x0800;
const EM_SETMARGINS: u32 = 0x00d3;
const EC_LEFTMARGIN_RIGHTMARGIN: usize = 0x3;
const EM_SETTEXTEX: u32 = 0x0400 + 97;
const EM_GETSCROLLPOS: u32 = 0x0400 + 221;
const EM_SETSCROLLPOS: u32 = 0x0400 + 222;
/// Pixels of margin on each side of the text.
const MARGIN: i32 = 12;
/// Most bytes read from a file (a preview, not an editor).
const MAX_BYTES: usize = 8 << 20;

/// `RECT`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// `POINT`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

/// `SETTEXTEX`.
#[repr(C)]
#[allow(dead_code)] // Read by RichEdit.
struct SetTextEx {
    flags: u32,
    codepage: u32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Hwnd,
        menu: *mut c_void,
        instance: *mut c_void,
        param: *const c_void,
    ) -> Hwnd;
    fn DestroyWindow(window: Hwnd) -> i32;
    fn MoveWindow(window: Hwnd, x: i32, y: i32, width: i32, height: i32, repaint: i32) -> i32;
    fn SetParent(window: Hwnd, parent: Hwnd) -> Hwnd;
    fn SendMessageW(window: Hwnd, message: u32, wparam: usize, lparam: isize) -> isize;
    fn SetFocus(window: Hwnd) -> Hwnd;
    fn GetDpiForWindow(window: Hwnd) -> u32;
    fn GetFocus() -> Hwnd;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
}

/// `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

// ── Vtables (read by Windows) ──────────────────────────────────────

#[repr(C)]
#[allow(dead_code)]
struct UnknownVtbl {
    query_interface: unsafe extern "system" fn(This, *const Guid, *mut This) -> Hresult,
    add_ref: unsafe extern "system" fn(This) -> u32,
    release: unsafe extern "system" fn(This) -> u32,
}

#[repr(C)]
#[allow(dead_code)]
struct PreviewVtbl {
    unknown: UnknownVtbl,
    set_window: unsafe extern "system" fn(This, Hwnd, *const Rect) -> Hresult,
    set_rect: unsafe extern "system" fn(This, *const Rect) -> Hresult,
    do_preview: unsafe extern "system" fn(This) -> Hresult,
    unload: unsafe extern "system" fn(This) -> Hresult,
    set_focus: unsafe extern "system" fn(This) -> Hresult,
    query_focus: unsafe extern "system" fn(This, *mut Hwnd) -> Hresult,
    translate_accelerator: unsafe extern "system" fn(This, *mut c_void) -> Hresult,
}

#[repr(C)]
#[allow(dead_code)]
struct InitStreamVtbl {
    unknown: UnknownVtbl,
    initialize: unsafe extern "system" fn(This, *mut c_void, u32) -> Hresult,
}

#[repr(C)]
#[allow(dead_code)]
struct InitFileVtbl {
    unknown: UnknownVtbl,
    initialize: unsafe extern "system" fn(This, *const u16, u32) -> Hresult,
}

#[repr(C)]
#[allow(dead_code)]
struct SiteVtbl {
    unknown: UnknownVtbl,
    set_site: unsafe extern "system" fn(This, *mut c_void) -> Hresult,
    get_site: unsafe extern "system" fn(This, *const Guid, *mut This) -> Hresult,
}

#[repr(C)]
#[allow(dead_code)]
struct OleWindowVtbl {
    unknown: UnknownVtbl,
    get_window: unsafe extern "system" fn(This, *mut Hwnd) -> Hresult,
    context_sensitive_help: unsafe extern "system" fn(This, i32) -> Hresult,
}

#[repr(C)]
#[allow(dead_code)]
struct ClassFactoryVtbl {
    unknown: UnknownVtbl,
    create_instance:
        unsafe extern "system" fn(This, *mut c_void, *const Guid, *mut This) -> Hresult,
    lock_server: unsafe extern "system" fn(This, i32) -> Hresult,
}

/// `IStream`, up to `Read` (from `ISequentialStream`).
#[repr(C)]
#[allow(dead_code)]
struct StreamVtbl {
    unknown: UnknownVtbl,
    read: unsafe extern "system" fn(This, *mut c_void, u32, *mut u32) -> Hresult,
}

// ── The handler ────────────────────────────────────────────────────

/// Live handlers, and `LockServer` locks (for [`DllCanUnloadNow`]).
static OBJECTS: AtomicUsize = AtomicUsize::new(0);
static LOCKS: AtomicUsize = AtomicUsize::new(0);

/// What a handler knows.
struct State {
    /// The document, once initialised.
    text: Option<String>,
    /// Explorer's window for the preview, and the area to fill.
    parent: Hwnd,
    rect: Rect,
    /// The rich-text control, while previewing.
    edit: Hwnd,
    /// The width (pixels) the document was last laid out for.
    laid_out_width: i32,
    /// The site Explorer set (held with a reference).
    site: This,
}

/// A preview handler: one pointer per interface (each interface pointer
/// Windows holds points at its field), then the reference count and state.
#[repr(C)]
struct Handler {
    preview: &'static PreviewVtbl,
    init_stream: &'static InitStreamVtbl,
    init_file: &'static InitFileVtbl,
    site: &'static SiteVtbl,
    window: &'static OleWindowVtbl,
    refs: AtomicU32,
    state: RefCell<State>,
}

const PREVIEW: usize = offset_of!(Handler, preview);
const INIT_STREAM: usize = offset_of!(Handler, init_stream);
const INIT_FILE: usize = offset_of!(Handler, init_file);
const SITE: usize = offset_of!(Handler, site);
const WINDOW: usize = offset_of!(Handler, window);

/// `IUnknown` for the interface at byte `OFFSET` of [`Handler`].
const fn unknown<const OFFSET: usize>() -> UnknownVtbl {
    UnknownVtbl {
        query_interface: query_interface::<OFFSET>,
        add_ref: add_ref::<OFFSET>,
        release: release::<OFFSET>,
    }
}

static PREVIEW_VTBL: PreviewVtbl = PreviewVtbl {
    unknown: unknown::<PREVIEW>(),
    set_window,
    set_rect,
    do_preview,
    unload,
    set_focus,
    query_focus,
    translate_accelerator,
};
static INIT_STREAM_VTBL: InitStreamVtbl = InitStreamVtbl {
    unknown: unknown::<INIT_STREAM>(),
    initialize: initialize_with_stream,
};
static INIT_FILE_VTBL: InitFileVtbl = InitFileVtbl {
    unknown: unknown::<INIT_FILE>(),
    initialize: initialize_with_file,
};
static SITE_VTBL: SiteVtbl = SiteVtbl {
    unknown: unknown::<SITE>(),
    set_site,
    get_site,
};
static WINDOW_VTBL: OleWindowVtbl = OleWindowVtbl {
    unknown: unknown::<WINDOW>(),
    get_window,
    context_sensitive_help,
};

impl Handler {
    fn new() -> Self {
        Self {
            preview: &PREVIEW_VTBL,
            init_stream: &INIT_STREAM_VTBL,
            init_file: &INIT_FILE_VTBL,
            site: &SITE_VTBL,
            window: &WINDOW_VTBL,
            refs: AtomicU32::new(1),
            state: RefCell::new(State {
                text: None,
                parent: ptr::null_mut(),
                rect: Rect::default(),
                edit: ptr::null_mut(),
                laid_out_width: 0,
                site: ptr::null_mut(),
            }),
        }
    }

    /// `QueryInterface`: the pointer to the interface `iid`, with a new
    /// reference.
    fn query(&self, iid: *const Guid, out: *mut This) -> Hresult {
        if out.is_null() || iid.is_null() {
            return E_POINTER;
        }
        // SAFETY: checked non-null; the caller passes a valid GUID.
        let iid = unsafe { *iid };
        let interface: *const c_void = if iid == IID_UNKNOWN || iid == IID_PREVIEW_HANDLER {
            ptr::addr_of!(self.preview).cast()
        } else if iid == IID_INITIALIZE_WITH_STREAM {
            ptr::addr_of!(self.init_stream).cast()
        } else if iid == IID_INITIALIZE_WITH_FILE {
            ptr::addr_of!(self.init_file).cast()
        } else if iid == IID_OBJECT_WITH_SITE {
            ptr::addr_of!(self.site).cast()
        } else if iid == IID_OLE_WINDOW {
            ptr::addr_of!(self.window).cast()
        } else {
            // SAFETY: checked non-null; the caller owns the out pointer.
            unsafe { *out = ptr::null_mut() };
            return E_NOINTERFACE;
        };
        self.refs.fetch_add(1, Ordering::Relaxed);
        // SAFETY: as above.
        unsafe { *out = interface.cast_mut() };
        S_OK
    }

    /// Drop one reference to `handler`; the last one frees it.
    ///
    /// # Safety
    /// `handler` is a live handler made by [`create_instance`].
    unsafe fn release(handler: *const Self) -> u32 {
        // SAFETY: live, per the contract.
        let left = unsafe { (*handler).refs.fetch_sub(1, Ordering::AcqRel) } - 1;
        if left == 0 {
            // SAFETY: made by `Box::into_raw`, and this was the last
            // reference.
            drop(unsafe { Box::from_raw(handler.cast_mut()) });
            OBJECTS.fetch_sub(1, Ordering::AcqRel);
        }
        left
    }

    /// Show the document in a rich-text control on the parent window.
    fn show(&self) -> Hresult {
        let mut state = self.state.borrow_mut();
        if state.parent.is_null() {
            return E_UNEXPECTED;
        }
        if state.edit.is_null() {
            let library = wide("Msftedit.dll");
            let class = wide("RICHEDIT50W");
            let Rect {
                left,
                top,
                right,
                bottom,
            } = state.rect;
            // SAFETY: NUL-terminated names; the parent is the window
            // Explorer gave; a system window class needs no instance.
            state.edit = unsafe {
                LoadLibraryW(library.as_ptr());
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    ptr::null(),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_VSCROLL
                        | ES_MULTILINE
                        | ES_AUTOVSCROLL
                        | ES_READONLY,
                    left,
                    top,
                    right - left,
                    bottom - top,
                    state.parent,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            };
            if state.edit.is_null() {
                return E_FAIL;
            }
        }
        drop(state);
        self.lay_out();
        S_OK
    }

    /// Put the document into the control, laid out for its width (tables
    /// fit it), keeping the scroll position.
    fn lay_out(&self) {
        let mut state = self.state.borrow_mut();
        if state.edit.is_null() {
            return;
        }
        let width = (state.rect.right - state.rect.left - 2 * MARGIN).max(0);
        state.laid_out_width = state.rect.right - state.rect.left;
        // SAFETY: a live window.
        let dpi = match unsafe { GetDpiForWindow(state.edit) } {
            0 => 96,
            dpi => dpi,
        };
        let width_twips = u32::try_from(width).unwrap_or(0) * 1440 / dpi;
        let rtf = crate::rtf::to_rtf(state.text.as_deref().unwrap_or_default(), width_twips);
        let mut bytes = rtf.into_bytes();
        bytes.push(0);
        let set_text = SetTextEx {
            flags: 0,
            codepage: 0,
        };
        let margins = (MARGIN as isize) | ((MARGIN as isize) << 16);
        let mut scroll = Point::default();
        // SAFETY: a live window; `set_text`, `scroll` and the NUL-terminated
        // RTF outlive the calls.
        unsafe {
            SendMessageW(
                state.edit,
                EM_GETSCROLLPOS,
                0,
                ptr::addr_of_mut!(scroll) as isize,
            );
            SendMessageW(
                state.edit,
                EM_SETMARGINS,
                EC_LEFTMARGIN_RIGHTMARGIN,
                margins,
            );
            SendMessageW(
                state.edit,
                EM_SETTEXTEX,
                ptr::addr_of!(set_text) as usize,
                bytes.as_ptr() as isize,
            );
            SendMessageW(
                state.edit,
                EM_SETSCROLLPOS,
                0,
                ptr::addr_of!(scroll) as isize,
            );
        }
    }

    /// Fit the control to the area Explorer gave (`state.rect`), laying the
    /// document out again when its width changed.
    fn fit(&self) {
        let state = self.state.borrow();
        if state.edit.is_null() {
            return;
        }
        let Rect {
            left,
            top,
            right,
            bottom,
        } = state.rect;
        let (edit, laid_out) = (state.edit, state.laid_out_width);
        drop(state);
        // SAFETY: a window this handler made.
        unsafe { MoveWindow(edit, left, top, right - left, bottom - top, 1) };
        if right - left != laid_out {
            self.lay_out();
        }
    }

    /// Destroy the rich-text control.
    fn hide(&self) {
        let mut state = self.state.borrow_mut();
        if !state.edit.is_null() {
            // SAFETY: a window this handler made.
            unsafe { DestroyWindow(state.edit) };
            state.edit = ptr::null_mut();
        }
    }

    /// Take the document's bytes (UTF-8, a BOM dropped).
    fn set_text(&self, bytes: &[u8]) -> Hresult {
        let mut state = self.state.borrow_mut();
        if state.text.is_some() {
            return E_UNEXPECTED;
        }
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        state.text = Some(String::from_utf8_lossy(bytes).into_owned());
        S_OK
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        self.hide();
        let site = std::mem::replace(&mut self.state.get_mut().site, ptr::null_mut());
        release_unknown(site);
    }
}

/// Release a COM pointer (if any).
fn release_unknown(unknown: This) {
    if unknown.is_null() {
        return;
    }
    // SAFETY: a COM object starts with its vtable, which starts with
    // IUnknown's; this releases the reference the caller held.
    unsafe {
        let vtbl = &**unknown.cast::<*const UnknownVtbl>();
        (vtbl.release)(unknown);
    }
}

/// The handler whose interface at byte `OFFSET` is `this`.
///
/// # Safety
/// `this` is that interface pointer of a live handler.
#[allow(clippy::cast_ptr_alignment)] // Back to the start of the `Handler`.
unsafe fn handler<'a, const OFFSET: usize>(this: This) -> &'a Handler {
    // SAFETY: per the contract, `this` is `OFFSET` bytes into a handler.
    unsafe { &*this.cast::<u8>().sub(OFFSET).cast::<Handler>() }
}

unsafe extern "system" fn query_interface<const OFFSET: usize>(
    this: This,
    iid: *const Guid,
    out: *mut This,
) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    unsafe { handler::<OFFSET>(this) }.query(iid, out)
}

unsafe extern "system" fn add_ref<const OFFSET: usize>(this: This) -> u32 {
    // SAFETY: as above.
    unsafe { handler::<OFFSET>(this) }
        .refs
        .fetch_add(1, Ordering::Relaxed)
        + 1
}

unsafe extern "system" fn release<const OFFSET: usize>(this: This) -> u32 {
    // SAFETY: as above; the reference released is the caller's.
    unsafe { Handler::release(handler::<OFFSET>(this)) }
}

unsafe extern "system" fn set_window(this: This, parent: Hwnd, rect: *const Rect) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    let handler = unsafe { handler::<PREVIEW>(this) };
    if rect.is_null() {
        return E_POINTER;
    }
    let edit = {
        let mut state = handler.state.borrow_mut();
        state.parent = parent;
        // SAFETY: checked non-null.
        state.rect = unsafe { *rect };
        state.edit
    };
    if !edit.is_null() {
        // SAFETY: a window this handler made, and Explorer's window.
        unsafe { SetParent(edit, parent) };
    }
    handler.fit();
    S_OK
}

unsafe extern "system" fn set_rect(this: This, rect: *const Rect) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    let handler = unsafe { handler::<PREVIEW>(this) };
    if rect.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked non-null.
    handler.state.borrow_mut().rect = unsafe { *rect };
    handler.fit();
    S_OK
}

unsafe extern "system" fn do_preview(this: This) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    unsafe { handler::<PREVIEW>(this) }.show()
}

unsafe extern "system" fn unload(this: This) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    let handler = unsafe { handler::<PREVIEW>(this) };
    handler.hide();
    handler.state.borrow_mut().text = None;
    S_OK
}

unsafe extern "system" fn set_focus(this: This) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    let handler = unsafe { handler::<PREVIEW>(this) };
    let edit = handler.state.borrow().edit;
    if edit.is_null() {
        return S_FALSE;
    }
    // SAFETY: a window this handler made.
    unsafe { SetFocus(edit) };
    S_OK
}

unsafe extern "system" fn query_focus(_this: This, out: *mut Hwnd) -> Hresult {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked non-null.
    unsafe { *out = GetFocus() };
    S_OK
}

unsafe extern "system" fn translate_accelerator(_this: This, _message: *mut c_void) -> Hresult {
    S_FALSE
}

unsafe extern "system" fn initialize_with_stream(
    this: This,
    stream: *mut c_void,
    _mode: u32,
) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    let handler = unsafe { handler::<INIT_STREAM>(this) };
    if stream.is_null() {
        return E_POINTER;
    }
    // SAFETY: `stream` is an `IStream`, whose vtable starts with
    // IUnknown's and then `Read`.
    let read = unsafe { (**stream.cast::<*const StreamVtbl>()).read };
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    while bytes.len() < MAX_BYTES {
        let mut got = 0u32;
        // SAFETY: `chunk` has room for the bytes asked for.
        let result = unsafe {
            read(
                stream,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &raw mut got,
            )
        };
        if result < 0 {
            return result;
        }
        bytes.extend_from_slice(&chunk[..(got as usize).min(chunk.len())]);
        if got == 0 || result == S_FALSE {
            break;
        }
    }
    handler.set_text(&bytes)
}

unsafe extern "system" fn initialize_with_file(
    this: This,
    path: *const u16,
    _mode: u32,
) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    let handler = unsafe { handler::<INIT_FILE>(this) };
    if path.is_null() {
        return E_POINTER;
    }
    // SAFETY: a NUL-terminated path; its length is found before reading.
    let path = unsafe {
        let mut len = 0;
        while *path.add(len) != 0 {
            len += 1;
        }
        std::ffi::OsString::from_wide(std::slice::from_raw_parts(path, len))
    };
    match std::fs::read(&path) {
        Ok(mut bytes) => {
            bytes.truncate(MAX_BYTES);
            handler.set_text(&bytes)
        }
        Err(_) => E_FAIL,
    }
}

unsafe extern "system" fn set_site(this: This, site: *mut c_void) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    let handler = unsafe { handler::<SITE>(this) };
    if !site.is_null() {
        // SAFETY: a COM object; the handler keeps a reference.
        unsafe {
            let vtbl = &**site.cast::<*const UnknownVtbl>();
            (vtbl.add_ref)(site);
        }
    }
    let old = std::mem::replace(&mut handler.state.borrow_mut().site, site);
    release_unknown(old);
    S_OK
}

unsafe extern "system" fn get_site(this: This, iid: *const Guid, out: *mut This) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    let handler = unsafe { handler::<SITE>(this) };
    if out.is_null() {
        return E_POINTER;
    }
    let site = handler.state.borrow().site;
    if site.is_null() {
        // SAFETY: checked non-null.
        unsafe { *out = ptr::null_mut() };
        return E_FAIL;
    }
    // SAFETY: the site is a live COM object the handler holds.
    unsafe {
        let vtbl = &**site.cast::<*const UnknownVtbl>();
        (vtbl.query_interface)(site, iid, out)
    }
}

unsafe extern "system" fn get_window(this: This, out: *mut Hwnd) -> Hresult {
    // SAFETY: Windows calls through the interface it was given.
    let handler = unsafe { handler::<WINDOW>(this) };
    if out.is_null() {
        return E_POINTER;
    }
    let parent = handler.state.borrow().parent;
    // SAFETY: checked non-null.
    unsafe { *out = parent };
    if parent.is_null() { E_FAIL } else { S_OK }
}

unsafe extern "system" fn context_sensitive_help(_this: This, _enter: i32) -> Hresult {
    E_NOTIMPL
}

// ── The class factory and the DLL's exports ───────────────────────

/// The class factory: a single static object.
#[repr(C)]
struct Factory {
    vtbl: &'static ClassFactoryVtbl,
}

static FACTORY_VTBL: ClassFactoryVtbl = ClassFactoryVtbl {
    unknown: UnknownVtbl {
        query_interface: factory_query_interface,
        add_ref: factory_add_ref,
        release: factory_release,
    },
    create_instance,
    lock_server,
};
static FACTORY: Factory = Factory {
    vtbl: &FACTORY_VTBL,
};

unsafe extern "system" fn factory_query_interface(
    this: This,
    iid: *const Guid,
    out: *mut This,
) -> Hresult {
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked non-null.
    let iid = unsafe { *iid };
    let found = iid == IID_UNKNOWN || iid == IID_CLASS_FACTORY;
    // SAFETY: checked non-null.
    unsafe { *out = if found { this } else { ptr::null_mut() } };
    if found { S_OK } else { E_NOINTERFACE }
}

/// The factory is static: references are not counted.
unsafe extern "system" fn factory_add_ref(_this: This) -> u32 {
    2
}

unsafe extern "system" fn factory_release(_this: This) -> u32 {
    1
}

unsafe extern "system" fn create_instance(
    _this: This,
    outer: *mut c_void,
    iid: *const Guid,
    out: *mut This,
) -> Hresult {
    if out.is_null() {
        return E_POINTER;
    }
    if !outer.is_null() {
        // SAFETY: checked non-null.
        unsafe { *out = ptr::null_mut() };
        return CLASS_E_NOAGGREGATION;
    }
    OBJECTS.fetch_add(1, Ordering::AcqRel);
    let handler: *const Handler = Box::into_raw(Box::new(Handler::new()));
    // SAFETY: just made; `query` adds the caller's reference, and the
    // release drops the one `new` started with.
    unsafe {
        let result = (*handler).query(iid, out);
        Handler::release(handler);
        result
    }
}

unsafe extern "system" fn lock_server(_this: This, lock: i32) -> Hresult {
    if lock == 0 {
        LOCKS.fetch_sub(1, Ordering::AcqRel);
    } else {
        LOCKS.fetch_add(1, Ordering::AcqRel);
    }
    S_OK
}

/// COM asks the DLL for the class factory of `clsid`.
///
/// # Safety
/// The pointers are valid, as COM guarantees.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const Guid,
    iid: *const Guid,
    out: *mut This,
) -> Hresult {
    if out.is_null() || clsid.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked non-null.
    if unsafe { *clsid } != CLSID_PREVIEW {
        // SAFETY: checked non-null.
        unsafe { *out = ptr::null_mut() };
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory = ptr::addr_of!(FACTORY).cast_mut().cast();
    // SAFETY: the static factory and the caller's pointers.
    unsafe { factory_query_interface(factory, iid, out) }
}

/// Whether COM may unload the DLL: no handlers alive and no locks.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> Hresult {
    if OBJECTS.load(Ordering::Acquire) == 0 && LOCKS.load(Ordering::Acquire) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}
