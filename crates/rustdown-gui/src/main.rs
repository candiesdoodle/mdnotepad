#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(target_arch = "wasm32")]
compile_error!("This is a native desktop app; web/wasm builds are not supported.");

use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use eframe::egui;

mod about;
mod app_actions;
mod app_identity;
mod app_panels;
mod app_scroll;
#[cfg(test)]
#[allow(clippy::float_cmp)]
#[allow(clippy::wildcard_imports)]
#[allow(clippy::field_reassign_with_default)]
#[allow(clippy::unchecked_time_subtraction)]
mod app_tests;
mod cli;
mod clipboard;
mod commands;
mod crash_report;
mod diagnostics;
mod dialog;
mod disk;
mod document;
mod edit_ops;
mod editor;
mod explorer;
mod explorer_fs;
mod explorer_tree;
mod explorer_ui;
mod focus_mode;
mod format;
mod format_toolbar;
mod goto_line;
mod icons;
mod line_ending;
mod list_continue;
mod live_format;
#[cfg(test)]
#[allow(clippy::float_cmp)]
mod live_format_tests;
mod markdown_fence;
mod md;
mod md_commands;
mod menu;
mod mermaid;
mod nav;
mod preferences;
mod print;
mod quick_open;
mod scroll_math;
mod search;
mod session;
mod spell;
mod startup_timing;
mod status_bar;
mod tab;
mod tab_strip;
mod table_edit;
#[cfg(test)]
mod test_harness;
#[cfg(test)]
#[allow(clippy::float_cmp)]
mod text_size_tests;
mod title_bar;
mod toast;
mod ui_style;
#[cfg(windows)]
mod win_ffi;
mod win_file_assoc;
mod win_single_instance;
#[cfg(windows)]
mod win_spell;
mod window_state;
mod windows_integration;

pub(crate) use commands::AppCommand;
pub(crate) use document::{Document, DocumentStats};
pub(crate) use search::{SearchState, find_match_count};
use tab::{Tab, TabId};

const DEBOUNCE: Duration = Duration::from_millis(150);
const DISK_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DISK_RELOAD_DEBOUNCE: Duration = Duration::from_millis(75);
const STATS_RECALC_DEBOUNCE: Duration = Duration::from_millis(120);
const ZOOM_STEP: f32 = 0.1;
const MIN_ZOOM_FACTOR: f32 = 0.5;
const MAX_ZOOM_FACTOR: f32 = 3.0;
const PANEL_EDGE_PADDING: f32 = 8.0;
const SCROLL_WHEEL_MULTIPLIER: f32 = 1.15;
const SIDE_BY_SIDE_SCROLL_LERP: f32 = 0.35;
const DIAGNOSTICS_DEFAULT_ITERATIONS: usize = 200;
const DIAGNOSTICS_DEFAULT_RUNS: usize = 1;
const AUTO_SAVE_INTERVAL: Duration = Duration::from_secs(30);
const AUTO_SAVE_RETRY: Duration = Duration::from_secs(5);

use cli::{DiagnosticsMode, parse_launch_options};

fn main() -> eframe::Result {
    let mut startup = startup_timing::StartupTiming::start();
    crash_report::install();
    let mut launch_options = parse_launch_options(std::env::args_os().skip(1));
    if launch_options.print_version {
        // On Windows, GUI-subsystem binaries have no console by default.
        // Attach to the parent console so the output is visible in
        // PowerShell / cmd.
        #[cfg(windows)]
        cli::attach_parent_console();

        println!("{}", app_identity::version_line());
        return Ok(());
    }
    if let Some(action) = launch_options.file_types {
        #[cfg(windows)]
        cli::attach_parent_console();
        std::process::exit(windows_integration::run_file_types_cli(action));
    }

    if launch_options.diagnostics == DiagnosticsMode::OpenPipeline {
        for run in 0..launch_options.diagnostics_runs {
            if launch_options.diagnostics_runs > 1 {
                println!(
                    "diagnostics_run={}/{}",
                    run + 1,
                    launch_options.diagnostics_runs
                );
            }
            if let Err(err) = diagnostics::run_open_pipeline_diagnostics(
                launch_options.path.as_deref(),
                launch_options.diagnostics_iterations,
            ) {
                eprintln!("Diagnostics failed: {err}");
                break;
            }
        }
        return Ok(());
    }
    #[cfg(debug_assertions)]
    if launch_options.diagnostics == DiagnosticsMode::NavPipeline {
        if let Err(err) = nav::debug::run_nav_diagnostics(launch_options.path.as_deref()) {
            eprintln!("Nav diagnostics failed: {err}");
        }
        return Ok(());
    }
    // Single instance (Windows): a running window takes the files instead.
    let inbox = match windows_integration::single_instance_startup(&launch_options) {
        windows_integration::Startup::Forwarded => return Ok(()),
        windows_integration::Startup::Run(inbox) => inbox,
    };
    let extra_paths = std::mem::take(&mut launch_options.extra_paths);
    startup.set_opens_file(launch_options.path.is_some() || !extra_paths.is_empty());
    let mut app = RustdownApp::from_launch_options(launch_options);
    for path in extra_paths {
        app.open_path_or_folder(path);
    }
    // The first window brings back the tabs of the last session.
    if inbox.is_some() {
        app.reopen_session();
    }
    startup.mark("files and settings read");

    // Viewport sizes are in points, so they scale with the OS DPI factor.
    // No OS title bar: `title_bar.rs` draws Notepad's (tabs, caption
    // buttons) and provides moving and resizing.  An empty icon keeps
    // eframe from giving the window egui's logo; on Windows the window
    // takes the exe's icon (`windows_integration::style_frameless_window`).
    // The window opens where it was last time, as big (or maximized).
    let options = eframe::NativeOptions {
        viewport: app.window.apply(
            egui::ViewportBuilder::default()
                .with_decorations(false)
                .with_icon(egui::IconData::default()),
        ),
        ..Default::default()
    };
    let result = eframe::run_native(
        app_identity::APP_NAME,
        options,
        Box::new(move |cc| {
            startup.mark("window created");
            egui_extras::install_image_loaders(&cc.egui_ctx);
            ui_style::configure_fonts(&cc.egui_ctx).map_err(std::io::Error::other)?;
            startup.mark("fonts read");
            ui_style::configure_style(&cc.egui_ctx);
            ui_style::apply_theme(&cc.egui_ctx, app.theme);
            // Directly, so the first frame is already zoomed (later
            // changes go through `sync_zoom`).
            cc.egui_ctx
                .options_mut(|options| options.zoom_factor = app.zoom);
            let hwnd = windows_integration::style_frameless_window(cc);
            app.windows.set_window(hwnd);
            app.attach_single_instance(inbox, cc);
            app.startup = Some(startup);
            Ok(Box::new(app))
        }),
    );
    if let Err(err) = &result {
        crash_report::report("The window could not be created.", &err.to_string());
    }
    startup_timing::log_close();
    result
}

#[must_use]
fn markdown_file_dialog() -> rfd::FileDialog {
    rfd::FileDialog::new()
        .add_filter("Markdown", &["md", "markdown"])
        .add_filter("Text", &["txt"])
        .add_filter("All files", &["*"])
}

/// Whether `path` is a plain text file (`.txt`): opened as plain text.
#[must_use]
fn is_text_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
}

#[must_use]
fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

#[must_use]
fn markdown_paths<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|path| is_markdown_path(path))
        .map(Path::to_path_buf)
        .collect()
}

#[must_use]
pub(crate) fn default_image_uri_scheme(path: Option<&Path>) -> String {
    let Some(parent) = path.and_then(Path::parent) else {
        return "file://".to_owned();
    };

    let needs_canonicalize = !parent.is_absolute()
        || parent
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir));
    let base = if needs_canonicalize {
        parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf())
    } else {
        parent.to_path_buf()
    };
    let mut normalized = base.to_string_lossy().replace('\\', "/");
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    if !normalized.ends_with('/') {
        normalized.push('/');
    }
    format!("file://{normalized}")
}

/// App-wide state.  Per-document state lives in [`Tab`].
#[allow(clippy::struct_excessive_bools)]
struct RustdownApp {
    /// Open documents.  Never empty: closing the last tab closes the window
    /// or leaves an empty Untitled tab (`close_window_with_last_tab`).
    tabs: Vec<Tab>,
    /// Index of the active tab in `tabs`; always in bounds.
    active: usize,

    mode: Mode,
    /// Find/replace bar.  App-wide (like Notepad's): the query survives tab
    /// switches and always applies to the active tab.
    search: SearchState,
    focus_search: bool,
    /// Outline panel visibility; a persisted preference shared by all tabs.
    nav_visible: bool,
    heading_color_mode: bool,
    side_by_side_scroll_sync: bool,
    auto_save: bool,
    /// Formatting controls in the menu row (View → Formatting toolbar).
    show_format_toolbar: bool,
    /// Explorer side panel on the left (Phase 2C fills it in).
    show_explorer: bool,
    show_about: bool,
    /// The license or third-party notices window over the About box.
    legal_text: Option<about::LegalText>,
    /// Set when the active tab changes: the tab strip scrolls it into view,
    /// then clears the flag.
    scroll_to_active_tab: bool,
    /// Set when the active tab changes: its editor takes keyboard focus the
    /// next time it is shown, then clears the flag.
    focus_editor: bool,

    /// Pending "save changes?" prompts for a tab close or a window close.
    close_prompt: Option<ClosePrompt>,
    /// Set once the user has answered every prompt; lets the next window
    /// close request through instead of cancelling it.
    allow_window_close: bool,
    /// Send `ViewportCommand::Close` at the end of this frame.
    window_close_pending: bool,

    last_viewport_title: String,

    /// Explorer folder tree (shown while `show_explorer` is set).
    explorer: explorer::ExplorerState,
    /// Single-instance inbox and file-association notice (Windows).
    windows: windows_integration::WindowsState,
    /// View → Theme (persisted).
    theme: ui_style::ThemeChoice,
    /// `dark_mode` of the visuals the editor colours were built for; the
    /// cached galleys are dropped when it changes (incl. System theme flips).
    last_dark_mode: Option<bool>,
    /// View → Word wrap (persisted).
    word_wrap: bool,
    /// View → Status bar (persisted).
    status_bar_visible: bool,
    /// Edit-menu work queued for the editor.
    editor_ops: edit_ops::EditorOps,
    /// Edit → Go to… dialog, while open.
    goto_dialog: Option<goto_line::GoToLineDialog>,
    /// Status-bar values cached between frames.
    status_cache: status_bar::StatusCache,
    /// Line-ending style of new Untitled tabs (CRLF on Windows; tests set it).
    untitled_line_ending: line_ending::EndOfLine,
    /// Settings → Close window with last tab (persisted).
    close_window_with_last_tab: bool,
    /// View → Zoom (persisted): scales the whole window (egui's zoom
    /// factor, see `sync_zoom`).
    zoom: f32,
    /// Cold-start timing, until the first frame is on screen.
    startup: Option<startup_timing::StartupTiming>,
    /// The window's position, size and maximized state (persisted).
    window: window_state::WindowState,
    /// View → Focus mode: full screen, only the document.
    focus_mode: bool,
    /// When focus mode was entered (egui time), while its hint shows.
    focus_entered_at: Option<f64>,
    /// File → Quick open, while its box is open.
    quick_open: Option<quick_open::QuickOpen>,
    /// About → Reset settings asked; waiting for the answer.
    confirm_reset: bool,
    /// What the session written last depended on (see `session`).
    session_signature: u64,
    /// File → Recent files, newest first (persisted).
    recent_files: Vec<PathBuf>,
    /// A pasted image waiting to be saved (see `clipboard`).
    pending_image: Option<clipboard::PendingImage>,
    /// The note showing (see `toast`): its text and when it appeared.
    toast: Option<(String, f64)>,
    /// Ctrl+V was down in the previous frame (see `clipboard`).
    #[cfg_attr(not(windows), allow(dead_code))]
    ctrl_v_down: bool,
    /// When the session last changed without being written (egui time).
    session_changed_at: Option<f64>,
    /// The mode and tab the document area showed in the previous frame.
    last_shown: Option<(Mode, TabId)>,
    /// When the document area last switched mode or tab (egui time), while
    /// it fades in.
    switched_at: Option<f64>,
    /// View → Spell check and its checker (see `spell`).
    spell: spell::SpellCheck,
    /// The table the editor's right-click menu is about: where the click
    /// was (byte) and the table there.
    table_menu: Option<(usize, table_edit::Table)>,
    /// The system fallback fonts are still to be added (after the first
    /// frame; see `ui_style::add_fallback_fonts`).
    fallback_fonts_pending: bool,
    /// File → Save as PDF: the PDF being made, until it is done.
    pdf_export: Option<std::sync::mpsc::Receiver<Result<PathBuf, String>>>,
}

impl Default for RustdownApp {
    fn default() -> Self {
        Self {
            tabs: vec![Tab::default()],
            active: 0,
            mode: Mode::default(),
            search: SearchState::default(),
            focus_search: false,
            nav_visible: false,
            heading_color_mode: false,
            side_by_side_scroll_sync: false,
            auto_save: false,
            show_format_toolbar: true,
            show_explorer: false,
            show_about: false,
            legal_text: None,
            scroll_to_active_tab: false,
            focus_editor: false,
            close_prompt: None,
            allow_window_close: false,
            window_close_pending: false,
            last_viewport_title: String::new(),
            explorer: explorer::ExplorerState::default(),
            windows: windows_integration::WindowsState::default(),
            theme: ui_style::ThemeChoice::default(),
            last_dark_mode: None,
            word_wrap: true,
            status_bar_visible: true,
            editor_ops: edit_ops::EditorOps::default(),
            goto_dialog: None,
            status_cache: status_bar::StatusCache::default(),
            untitled_line_ending: line_ending::EndOfLine::NEW_DOCUMENT,
            close_window_with_last_tab: false,
            zoom: 1.0,
            startup: None,
            window: window_state::WindowState::default(),
            focus_mode: false,
            focus_entered_at: None,
            quick_open: None,
            confirm_reset: false,
            session_signature: 0,
            recent_files: Vec::new(),
            ctrl_v_down: false,
            toast: None,
            pending_image: None,
            session_changed_at: None,
            last_shown: None,
            switched_at: None,
            spell: spell::SpellCheck::default(),
            table_menu: None,
            fallback_fonts_pending: !cfg!(test),
            pdf_export: None,
        }
    }
}

/// Dirty tabs waiting for a Save / Don't save / Cancel answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ClosePrompt {
    /// Tabs still to ask about, front first.
    queue: Vec<TabId>,
    /// `true` when raised by a window close (then the window closes once the
    /// queue drains); `false` for closing a single tab.
    closing_window: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseChoice {
    Save,
    DontSave,
    Cancel,
}

/// How the active document is shown.  The status-bar switch, the View
/// menu and Ctrl+Enter go through them in [`Mode::ALL`] order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    /// The formatted (live-preview) editor (`live_format.rs`).
    #[default]
    Edit,
    /// Plain text, as Notepad shows a `.md` file: one monospace font, one
    /// size, one colour (`editor::raw_sections`).
    Raw,
    /// The raw editor on the left, the preview on the right.
    SideBySide,
    Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SideBySideScrollSource {
    Editor,
    Preview,
}

impl Mode {
    /// Every mode, in switch / menu / Ctrl+Enter order.
    const ALL: [Self; 4] = [Self::Edit, Self::Raw, Self::SideBySide, Self::Preview];

    /// Returns `true` when this mode renders the code editor.
    #[must_use]
    const fn uses_editor(self) -> bool {
        !matches!(self, Self::Preview)
    }

    /// The next mode for Ctrl+Enter: Edit, Raw, Split, Preview, Edit, ...
    #[must_use]
    const fn cycle(self) -> Self {
        match self {
            Self::Edit => Self::Raw,
            Self::Raw => Self::SideBySide,
            Self::SideBySide => Self::Preview,
            Self::Preview => Self::Edit,
        }
    }

    /// Label of the status-bar mode switch and of the View menu item.
    #[must_use]
    const fn short_label(self) -> &'static str {
        match self {
            Self::Edit => "Edit",
            Self::Raw => "Raw",
            Self::SideBySide => "Split",
            Self::Preview => "Preview",
        }
    }

    #[must_use]
    const fn tooltip(self) -> &'static str {
        match self {
            Self::Edit => "Edit: formatted text",
            Self::Raw => "Raw: the Markdown as plain text",
            Self::SideBySide => "Split: raw text and preview side by side",
            Self::Preview => "Preview",
        }
    }

    /// The persisted name (`mode` in the settings file).
    #[must_use]
    const fn as_str(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Raw => "raw",
            Self::SideBySide => "sidebyside",
            Self::Preview => "preview",
        }
    }

    #[must_use]
    fn from_str_lossy(s: &str) -> Option<Self> {
        match s {
            "edit" => Some(Self::Edit),
            "raw" => Some(Self::Raw),
            "sidebyside" | "split" => Some(Self::SideBySide),
            "preview" => Some(Self::Preview),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BundledDoc {
    Demo,
    Verification,
}

impl BundledDoc {
    const fn content(self) -> &'static str {
        match self {
            Self::Demo => include_str!("bundled/demo.md"),
            Self::Verification => include_str!("bundled/verification.md"),
        }
    }
}

/// `zoom_factor` within the zoom range (100 % when it is not a number).
#[must_use]
const fn clamped_zoom_factor(zoom_factor: f32) -> f32 {
    if zoom_factor.is_nan() {
        return 1.0;
    }
    zoom_factor.clamp(MIN_ZOOM_FACTOR, MAX_ZOOM_FACTOR)
}

/// `current_zoom` plus `delta`, to the whole percent (so ten steps of 10 %
/// land on 200 %, not 200.00002 %).
#[must_use]
fn zoom_with_step(current_zoom: f32, delta: f32) -> f32 {
    clamped_zoom_factor(((current_zoom + delta) * 100.0).round() / 100.0)
}

#[must_use]
fn zoom_with_factor(current_zoom: f32, factor: f32) -> f32 {
    if !factor.is_finite() || factor <= 0.0 {
        return clamped_zoom_factor(current_zoom);
    }
    clamped_zoom_factor(current_zoom * factor)
}

impl eframe::App for RustdownApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self
            .startup
            .as_mut()
            .is_some_and(|startup| startup.frame(ctx))
        {
            self.startup = None;
        }
        // Once the window is up: the fonts for emoji, symbols and other
        // scripts, then the documents laid out again with them.
        if self.fallback_fonts_pending && self.startup.is_none() {
            self.fallback_fonts_pending = false;
            if ui_style::add_fallback_fonts(ctx) {
                self.drop_colour_caches();
            }
        }
        self.sync_zoom(ctx);
        self.sync_theme_caches(ctx);
        self.handle_close_request(ctx);
        self.windows_integration_frame(ctx);
        self.tick_disk_sync(ctx);
        self.tick_auto_save(ctx);
        self.tick_session(ctx);
        self.tab_mut().refresh_stats_if_due(ctx);
        self.tick_spelling(ctx);
        self.poll_pdf_export(ctx);
        self.handle_keyboard_shortcuts(ctx);
        self.watch_image_paste(ctx);

        // Panel order matters: top panels stack downwards, bottom panels
        // upwards, side panels inwards, then the central panel fills the rest.
        // Row 1: icon, tabs, caption buttons.  Row 2: menus, formatting
        // controls, settings.  Focus mode shows only the document.
        if !self.focus_mode {
            self.show_title_bar(ctx);
            self.show_menu_bar(ctx);
            self.show_status_bar(ctx);
        }
        if self.search.visible {
            self.show_search_bar(ctx);
        }
        if !self.focus_mode {
            self.show_explorer_panel(ctx);
        }
        self.show_content_panels(ctx);

        self.show_goto_dialog(ctx);
        self.show_close_prompt(ctx);
        self.show_disk_change_dialog(ctx);
        self.show_about_window(ctx);
        self.show_reset_settings_prompt(ctx);
        self.show_image_paste_dialog(ctx);
        self.show_quick_open(ctx);
        self.show_focus_hint(ctx);
        self.show_toast(ctx);
        if !self.focus_mode {
            title_bar::show_resize_edges(ctx);
        }
        self.track_window_state(ctx);
        self.update_viewport_title(ctx);
        self.flush_window_close(ctx);
    }

    /// Opaque, so nothing shows through where the frameless window's DWM
    /// frame reaches into the client area (`win_ffi::enable_frame_shadow`).
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}
