//! Windows shell integration: `.md` file association and single instance.
//!
//! The testable parts live in `win_file_assoc` (registry plan) and
//! `win_single_instance` (protocol, election, server); this module wires
//! them into commands, the settings menu, startup and the frame loop.  On other
//! targets (the Linux dev/test host) the commands are disabled and single
//! instance is off.

use std::io::Write as _;

use eframe::egui::{self, KeyboardShortcut};

use crate::{
    RustdownApp,
    app_identity::{APP_NAME, app_name},
    cli::{FileTypesAction, LaunchOptions},
    commands::AppCommand,
    ui_style, win_file_assoc,
    win_single_instance::Inbox,
};

/// Windows shell integration commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowsCommand {
    /// Register as a `.md` / `.markdown` handler, then open Settings →
    /// Default apps so the user can confirm the app as the default.
    RegisterFileTypes,
    /// Remove that registration.
    UnregisterFileTypes,
}

/// Key bindings for [`WindowsCommand`]; merged into `commands::shortcut_table`.
pub(crate) const fn windows_shortcuts() -> Vec<(KeyboardShortcut, AppCommand)> {
    Vec::new()
}

/// The settings-menu items of [`RustdownApp::windows_help_menu_items`].
pub(crate) const WINDOWS_MENU_ITEMS: [(&str, WindowsCommand); 2] = [
    (
        concat!("Make ", app_name!(), " the default for .md and .txt files…"),
        WindowsCommand::RegisterFileTypes,
    ),
    (
        "Remove .md and .txt file associations",
        WindowsCommand::UnregisterFileTypes,
    ),
];

const REGISTERED: &str = concat!(app_name!(), " can now open .md, .markdown and .txt files.");
const CONFIRM_IN_SETTINGS: &str = concat!(
    "Windows does not let apps make themselves the default. ",
    "In the Settings page that just opened, choose ",
    app_name!(),
    " for .md (and .markdown) and .txt files."
);
const OPEN_SETTINGS: &str = concat!(
    "Windows does not let apps make themselves the default. ",
    "Open Settings > Apps > Default apps > ",
    app_name!(),
    " and choose it for .md (and .markdown) and .txt files."
);
const UNREGISTERED: &str = concat!(
    app_name!(),
    " is no longer registered for .md, .markdown and .txt files. ",
    "If it was the default, Windows asks which app to use the next time you open one."
);

/// Windows integration state of the app (a field of [`RustdownApp`]).
#[derive(Default)]
pub(crate) struct WindowsState {
    /// Requests from later launches; `Some` only in the primary instance.
    inbox: Option<Inbox>,
    /// Outcome of the last file-association command, shown until dismissed.
    notice: Option<String>,
    /// The notice is the one after registering (`Some(settings opened)`):
    /// it is rebuilt when the window regains focus, so the defaults it
    /// lists are the ones the user just chose in Settings.
    association_notice: Option<bool>,
    /// The main window's `HWND`, 0 if unknown (or not on Windows).
    #[cfg_attr(not(windows), allow(dead_code))]
    hwnd: isize,
    /// Whether the window had keyboard focus in the previous frame.
    focused: bool,
    /// The accent colour the visuals were last given.
    accent: Option<ui_style::Accent>,
}

impl WindowsState {
    /// Whether this is the first window (it receives later launches'
    /// files, and keeps the session).
    pub(crate) const fn is_primary(&self) -> bool {
        self.inbox.is_some()
    }

    /// Remember the main window's `HWND` (from [`style_frameless_window`]).
    pub(crate) const fn set_window(&mut self, hwnd: isize) {
        self.hwnd = hwnd;
    }

    /// The accent colour the visuals were last given.
    pub(crate) const fn accent(&self) -> Option<ui_style::Accent> {
        self.accent
    }

    /// Give the window's backdrop and border their dark (or light) variant.
    pub(crate) fn set_dark_frame(&self, dark: bool) {
        #[cfg(windows)]
        if self.hwnd != 0 {
            crate::win_ffi::set_dark_frame(self.hwnd, dark);
        }
        #[cfg(not(windows))]
        let _ = (self, dark);
    }
}

/// What `main` does after the single-instance check.
pub(crate) enum Startup {
    /// A running window of the app took this launch's files: exit.
    #[cfg_attr(not(windows), allow(dead_code))] // single instance is Windows-only
    Forwarded,
    /// Open a window; `Some` inbox when it is the primary instance.
    Run(Option<Inbox>),
}

/// The notice after registering: what it did and, for each file type,
/// whether the app is the default now (read from Windows, so it says
/// what the user has really chosen).  `settings_opened`: the Default apps
/// page opened.
fn association_notice(settings_opened: bool) -> String {
    use std::fmt::Write as _;
    let defaults = win_file_assoc::defaults();
    if !defaults.is_empty() && defaults.iter().all(|entry| entry.ours) {
        return format!("{APP_NAME} is the default app for .md, .markdown and .txt files.");
    }
    let how = if settings_opened {
        CONFIRM_IN_SETTINGS
    } else {
        OPEN_SETTINGS
    };
    let mut text = format!("{REGISTERED}\n\n{how}");
    if !defaults.is_empty() {
        text.push_str("\n\nDefault app now:");
        for entry in &defaults {
            let app = if entry.ours {
                APP_NAME
            } else {
                entry.app.as_deref().unwrap_or("none")
            };
            let _ = write!(text, "\n    {}: {app}", entry.extension);
        }
    }
    text
}

/// Single instance: hand this launch's files to an already running window,
/// or become the window that later launches hand theirs to.
#[cfg(windows)]
pub(crate) fn single_instance_startup(options: &LaunchOptions) -> Startup {
    use crate::win_single_instance::{
        Election, OpenRequest, Timing, elect, pipe::PipeTransport, spawn_server,
    };

    let request = (!options.new_window).then(|| {
        // Without a working directory relative paths stay relative and the
        // request is not sent (we open our own window instead).
        let cwd = std::env::current_dir().unwrap_or_default();
        let paths = options.path.iter().chain(&options.extra_paths).cloned();
        OpenRequest::from_args(paths, &cwd)
    });
    let transport = PipeTransport::for_current_session();
    match elect(&transport, request.as_ref(), Timing::DEFAULT) {
        Election::Forwarded => Startup::Forwarded,
        Election::Primary(listener) => Startup::Run(spawn_server(listener).ok()),
        Election::Standalone => Startup::Run(None),
    }
}

/// Single instance is Windows-only: always open a window.
#[cfg(not(windows))]
pub(crate) const fn single_instance_startup(_options: &LaunchOptions) -> Startup {
    Startup::Run(None)
}

/// `--register-file-types` / `--unregister-file-types`: apply, print the
/// outcome, and return the process exit code.
pub(crate) fn run_file_types_cli(action: FileTypesAction) -> i32 {
    use win_file_assoc::Registration;
    let (result, done) = match action {
        FileTypesAction::Register => (
            win_file_assoc::register(Registration::FileTypes),
            format!(
                "{REGISTERED}\nTo make it the default: Settings > Apps > Default apps > {APP_NAME}\n\
                 (or run: start {})",
                win_file_assoc::DEFAULT_APPS_URI
            ),
        ),
        FileTypesAction::Unregister => (win_file_assoc::unregister(false), UNREGISTERED.to_owned()),
        FileTypesAction::RegisterPreview => (
            win_file_assoc::register(Registration::Preview),
            format!("{APP_NAME} now shows .md files in File Explorer's preview pane."),
        ),
        FileTypesAction::UnregisterPreview => (
            win_file_assoc::unregister(true),
            format!("{APP_NAME} no longer shows .md files in File Explorer's preview pane."),
        ),
    };
    match result {
        Ok(()) => {
            let _ = writeln!(std::io::stdout(), "{done}");
            0
        }
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "{APP_NAME}: {err}");
            1
        }
    }
}

/// Native window handle (`HWND`) of the new window, 0 if unknown.
#[cfg(windows)]
fn native_window_handle(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match cc.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
        _ => 0,
    }
}

#[cfg(not(windows))]
const fn native_window_handle(_cc: &eframe::CreationContext<'_>) -> isize {
    0
}

/// Whether a window at `position` (its top-left corner) with inner size
/// `size` would show on a screen: the middle of its title bar must be on a
/// monitor (one may have been unplugged since).
#[cfg(windows)]
#[must_use]
pub(crate) fn on_a_screen(position: [f32; 2], size: [f32; 2]) -> bool {
    /// How far below the window's top the title bar is grabbed.
    const TITLE_BAR_GRIP: f32 = 16.0;
    crate::win_ffi::point_on_a_monitor(
        size[0].mul_add(0.5, position[0]),
        position[1] + TITLE_BAR_GRIP,
    )
}

/// Off Windows (the dev host): trust the saved position.
#[cfg(not(windows))]
#[must_use]
pub(crate) const fn on_a_screen(_position: [f32; 2], _size: [f32; 2]) -> bool {
    true
}

/// The user's Windows accent colour, `None` when Windows does not say.
#[cfg(windows)]
#[must_use]
pub(crate) fn read_accent() -> Option<ui_style::Accent> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let palette = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent")
        .ok()?
        .get_raw_value("AccentPalette")
        .ok()?;
    ui_style::Accent::from_palette(&palette.bytes)
}

#[cfg(not(windows))]
#[must_use]
pub(crate) const fn read_accent() -> Option<ui_style::Accent> {
    None
}

/// Windows 11 look for the frameless main window (it has no OS title bar,
/// see `title_bar.rs`): animations as Windows' setting says, rounded
/// corners, the DWM drop shadow, and the exe's icon for the taskbar and
/// Alt+Tab (`main` keeps eframe from setting its own).  Called once from
/// the eframe app creator; returns the window's `HWND` (0 if unknown).
#[cfg(windows)]
pub(crate) fn style_frameless_window(cc: &eframe::CreationContext<'_>) -> isize {
    ui_style::set_motion(crate::win_ffi::animation_effects_enabled());
    let hwnd = native_window_handle(cc);
    if hwnd != 0 {
        crate::win_ffi::round_window_corners(hwnd);
        crate::win_ffi::enable_frame_shadow(hwnd);
        crate::win_ffi::set_window_icons_from_exe(hwnd);
    }
    hwnd
}

/// The Linux dev host draws no window frame to style.
#[cfg(not(windows))]
pub(crate) const fn style_frameless_window(_cc: &eframe::CreationContext<'_>) -> isize {
    0
}

impl RustdownApp {
    /// Run a [`WindowsCommand`].
    pub(crate) fn execute_windows(&mut self, cmd: WindowsCommand, ctx: &egui::Context) {
        self.windows.association_notice = None;
        let notice = match cmd {
            WindowsCommand::RegisterFileTypes => {
                match win_file_assoc::register(win_file_assoc::Registration::Everything) {
                    Ok(()) => {
                        let settings_opened = win_file_assoc::open_default_apps_settings();
                        self.windows.association_notice = Some(settings_opened);
                        association_notice(settings_opened)
                    }
                    Err(err) => {
                        format!("Could not register {APP_NAME} for .md and .txt files.\n\n{err}")
                    }
                }
            }
            WindowsCommand::UnregisterFileTypes => match win_file_assoc::unregister(false) {
                Ok(()) => UNREGISTERED.to_owned(),
                Err(err) => format!("Could not remove the .md file association.\n\n{err}"),
            },
        };
        self.windows.notice = Some(notice);
        ctx.request_repaint();
    }

    /// Windows integration items in the settings menu (disabled elsewhere).
    pub(crate) fn windows_help_menu_items(&mut self, ui: &mut egui::Ui) {
        crate::menu::separator(ui);
        for (label, cmd) in WINDOWS_MENU_ITEMS {
            // Shared menu helper so the label lines up with the other items.
            ui.add_enabled_ui(win_file_assoc::SUPPORTED, |ui| {
                self.menu_item(ui, label, AppCommand::Windows(cmd));
            });
        }
    }

    /// Connect the single-instance server (if this is the primary instance)
    /// to the new window; called once from the eframe app creator.
    pub(crate) fn attach_single_instance(
        &mut self,
        inbox: Option<Inbox>,
        cc: &eframe::CreationContext<'_>,
    ) {
        if let Some(inbox) = &inbox {
            inbox.attach_ui(cc.egui_ctx.clone(), native_window_handle(cc));
        }
        self.windows.inbox = inbox;
    }

    /// Once per frame: open files sent by later launches and show the
    /// file-association notice.
    pub(crate) fn windows_integration_frame(&mut self, ctx: &egui::Context) {
        self.open_forwarded_files(ctx);
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        let regained = focused && !self.windows.focused;
        self.windows.focused = focused;
        if regained && let Some(settings_opened) = self.windows.association_notice {
            // Back from Settings: list the defaults as they are now.
            self.windows.notice = Some(association_notice(settings_opened));
        }
        self.show_windows_notice(ctx);
        self.follow_accent_colour(ctx, regained);
    }

    /// Use the Windows accent colour: read it in the first frame, and again
    /// whenever the window regains focus (it is changed in Settings, so the
    /// window has lost focus meanwhile).
    fn follow_accent_colour(&mut self, ctx: &egui::Context, regained: bool) {
        if !regained && self.windows.accent.is_some() {
            return;
        }
        let accent = read_accent().unwrap_or(ui_style::Accent::DEFAULT);
        if self.windows.accent.replace(accent) != Some(accent) {
            ui_style::apply_accent(ctx, accent);
            self.drop_colour_caches();
        }
    }

    /// Open each forwarded path in a tab (or focus its tab); an empty
    /// request opens a new tab, like launching Notepad again.  Then bring
    /// the window to the front.
    fn open_forwarded_files(&mut self, ctx: &egui::Context) {
        let Some(inbox) = &self.windows.inbox else {
            return;
        };
        let requests: Vec<_> = inbox.requests.try_iter().collect();
        if requests.is_empty() {
            return;
        }
        for request in requests {
            if request.paths.is_empty() {
                self.new_tab();
            }
            for path in request.paths {
                self.open_path_or_folder(path);
            }
        }
        // winit ignores Focus while minimized, so restore first.
        if ctx.input(|input| input.viewport().minimized) == Some(true) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn show_windows_notice(&mut self, ctx: &egui::Context) {
        let Some(notice) = &self.windows.notice else {
            return;
        };
        let notice = notice.clone();
        let dismissed = crate::dialog::show(
            ctx,
            "file_association_notice",
            "File types",
            |ui| {
                for paragraph in notice.split("\n\n") {
                    ui.label(paragraph);
                }
            },
            &[("OK", crate::dialog::Role::Primary)],
            Some(0),
        )
        .is_some();
        if dismissed {
            self.windows.notice = None;
            self.windows.association_notice = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::mpsc};

    use super::*;
    use crate::win_single_instance::{Listener, OpenRequest, spawn_server};

    /// A listener whose clients never come; the server thread ends at once.
    struct NoClients;

    impl Listener for NoClients {
        type Conn = std::io::Cursor<Vec<u8>>;

        fn accept(&mut self) -> std::io::Result<Self::Conn> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    /// An app that is the primary instance, plus a way to inject requests
    /// as if they came over the pipe.
    fn primary_app() -> (RustdownApp, mpsc::Sender<OpenRequest>) {
        let inbox = spawn_server(NoClients);
        assert!(inbox.is_ok());
        let Ok(mut inbox) = inbox else { unreachable!() };
        let (tx, rx) = mpsc::channel();
        inbox.requests = rx;
        let app = RustdownApp {
            windows: WindowsState {
                inbox: Some(inbox),
                ..WindowsState::default()
            },
            ..RustdownApp::default()
        };
        (app, tx)
    }

    fn frame(app: &mut RustdownApp, ctx: &egui::Context) -> Vec<egui::ViewportCommand> {
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            app.windows_integration_frame(ctx);
        });
        output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|viewport| viewport.commands.clone())
            .unwrap_or_default()
    }

    fn temp_file(name: &str, text: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("rustdown-win-{}-{name}", std::process::id()));
        assert!(std::fs::write(&path, text).is_ok());
        path
    }

    #[test]
    fn forwarded_paths_open_in_tabs_and_focus_the_window() {
        let ctx = egui::Context::default();
        let (mut app, tx) = primary_app();
        assert!(frame(&mut app, &ctx).is_empty(), "nothing to do yet");

        let a = temp_file("a.md", "# A");
        let b = temp_file("b.md", "# B");
        let request = OpenRequest {
            paths: vec![a.clone(), b.clone()],
        };
        assert!(tx.send(request).is_ok());
        let commands = frame(&mut app, &ctx);
        assert!(commands.contains(&egui::ViewportCommand::Focus));
        // The untouched Untitled tab is replaced, as Notepad does.
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active, 1);

        // Sending an open file again focuses its tab instead of reopening it.
        assert!(
            tx.send(OpenRequest {
                paths: vec![a.clone()],
            })
            .is_ok()
        );
        let commands = frame(&mut app, &ctx);
        assert!(commands.contains(&egui::ViewportCommand::Focus));
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active, 0);

        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[test]
    fn launch_without_paths_opens_a_new_tab() {
        let ctx = egui::Context::default();
        let (mut app, tx) = primary_app();
        assert!(tx.send(OpenRequest::default()).is_ok());
        assert!(tx.send(OpenRequest::default()).is_ok());
        let commands = frame(&mut app, &ctx);
        assert!(commands.contains(&egui::ViewportCommand::Focus));
        assert_eq!(app.tabs.len(), 3);
        assert_eq!(app.active, 2);
    }

    #[test]
    fn standalone_window_ignores_the_inbox_hook() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp::default();
        assert!(frame(&mut app, &ctx).is_empty());
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn notice_shows_until_dismissed() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp::default();
        app.windows.notice = Some("hello".to_owned());
        let _ = frame(&mut app, &ctx);
        assert!(app.windows.notice.is_some());
        let escape = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(escape, |ctx| app.windows_integration_frame(ctx));
        assert!(app.windows.notice.is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn file_association_commands_report_unsupported_off_windows() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp::default();
        app.execute(AppCommand::Windows(WindowsCommand::RegisterFileTypes), &ctx);
        assert!(
            app.windows
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("only available on Windows"))
        );
        assert_eq!(run_file_types_cli(FileTypesAction::Unregister), 1);
    }
}
