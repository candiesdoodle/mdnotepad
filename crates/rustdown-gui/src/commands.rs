//! Command seam: every user action — menu item, toolbar button, keyboard
//! shortcut — is an [`AppCommand`] run through [`RustdownApp::execute`].
//!
//! To add a command: add a variant, handle it in `execute`, and optionally
//! bind a key in [`shortcut_table`].  Menu items show the bound shortcut
//! automatically via [`AppCommand::shortcut`].

use std::path::PathBuf;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

use crate::{
    BundledDoc, Mode, RustdownApp, ZOOM_STEP, edit_ops, explorer::ExplorerCommand,
    format_toolbar::FormatCommand, tab::TabId, ui_style::ThemeChoice,
    windows_integration::WindowsCommand,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppCommand {
    // ── File ──
    /// Append an empty Untitled tab.
    NewTab,
    /// Show the Open dialog.
    OpenDialog,
    /// Open a file in a tab (or focus the tab that already has it).
    OpenPath(PathBuf),
    Save,
    SaveAs,
    /// Save every dirty tab.
    SaveAll,
    /// File → Print… (in the browser's print dialog).
    Print,
    /// File → Save as PDF….
    ExportPdf,
    /// Close the active tab (prompts if dirty).
    CloseActiveTab,
    /// Close a specific tab (prompts if dirty).
    CloseTab(TabId),
    /// Close the window (prompts for every dirty tab).
    Exit,

    // ── Tabs ──
    NextTab,
    PrevTab,
    ActivateTab(TabId),

    // ── Edit ──
    // Undo … Select all act on the active editor; see `edit_ops`.
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    /// Insert the local time and date at the caret (F5).
    InsertTimeDate,
    Find,
    /// Select the next / previous match of the find-bar query (F3 / Shift+F3).
    FindNext,
    FindPrevious,
    Replace,
    CloseSearch,
    /// Show the "Go to line" dialog.
    GoToLine,
    FormatDocument,

    // ── View ──
    SetMode(Mode),
    CycleMode,
    ToggleOutline,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleHeadingColors,
    ToggleLinkedScrolling,
    ToggleStatusBar,
    ToggleWordWrap,
    /// View → Spell check.
    ToggleSpellCheck,
    ToggleFormatToolbar,
    ToggleCloseWindowWithLastTab,
    /// View → Focus mode (F11).
    ToggleFocusMode,
    /// Edit → Copy as formatted text (Ctrl+Shift+C).
    CopyFormatted,
    /// Edit → Paste image.
    PasteImage,
    /// File → Recent files → Clear recent files.
    ClearRecentFiles,
    /// Drag a tab: move tab `id` to position `to`.
    MoveTab {
        id: TabId,
        to: usize,
    },
    /// File → Quick open (Ctrl+P).
    QuickOpen,
    SetTheme(ThemeChoice),

    // ── Window (custom title bar) ──
    /// Minimise the window (caption button).
    MinimizeWindow,
    /// Maximise the window, or restore it when maximised (caption button,
    /// title-bar double-click).
    ToggleMaximizeWindow,

    // ── Settings menu (gear) ──
    OpenBundled(BundledDoc),
    ShowAbout,

    // ── Feature groups, each owned by its own module ──
    /// Markdown formatting (`format_toolbar.rs`).
    Format(FormatCommand),
    /// Folder tree (`explorer.rs`).
    Explorer(ExplorerCommand),
    /// Windows shell integration (`windows_integration.rs`).
    Windows(WindowsCommand),
}

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const CMD_ALT: Modifiers = Modifiers::COMMAND.plus(Modifiers::ALT);

/// All key bindings: the core table plus each feature group's table.
///
/// egui matches modifiers logically (extra Shift/Alt are ignored), so
/// `Ctrl+Shift+T` must be tried before `Ctrl+T`.  The stable sort by modifier
/// count guarantees that across groups and otherwise keeps table order.
pub(crate) fn shortcut_table() -> Vec<(KeyboardShortcut, AppCommand)> {
    let mut table = bound_shortcuts();
    table.sort_by_key(|(shortcut, _)| std::cmp::Reverse(modifier_count(shortcut.modifiers)));
    table
}

/// Every group's bindings in table order (core first), unsorted.
fn bound_shortcuts() -> Vec<(KeyboardShortcut, AppCommand)> {
    core_shortcuts()
        .into_iter()
        .chain(crate::format_toolbar::format_shortcuts())
        .chain(crate::explorer::explorer_shortcuts())
        .chain(crate::windows_integration::windows_shortcuts())
        .collect()
}

fn modifier_count(modifiers: Modifiers) -> u8 {
    u8::from(modifiers.alt)
        + u8::from(modifiers.shift)
        + u8::from(modifiers.command || modifiers.ctrl || modifiers.mac_cmd)
}

/// Core key bindings (file, tabs, edit, view, settings).
///
/// Ctrl+B / Ctrl+I / Ctrl+K / Ctrl+E / Ctrl+1–6 and Ctrl+Shift+letters are
/// left free for the formatting group.
const fn core_shortcuts() -> [(KeyboardShortcut, AppCommand); 29] {
    let key = KeyboardShortcut::new;
    [
        (key(CMD_SHIFT, Key::S), AppCommand::SaveAs),
        (key(CMD_ALT, Key::S), AppCommand::SaveAll),
        (key(CMD_SHIFT, Key::T), AppCommand::ToggleOutline),
        (key(CMD_SHIFT, Key::Tab), AppCommand::PrevTab),
        // Notepad's Ctrl+H comes first so menus show it; Ctrl+Shift+F stays.
        (key(CMD, Key::H), AppCommand::Replace),
        (key(CMD_SHIFT, Key::F), AppCommand::Replace),
        (key(CMD_ALT, Key::F), AppCommand::FormatDocument),
        (
            key(CMD_SHIFT, Key::F11),
            AppCommand::OpenBundled(BundledDoc::Demo),
        ),
        (
            key(CMD_SHIFT, Key::F12),
            AppCommand::OpenBundled(BundledDoc::Verification),
        ),
        (key(Modifiers::SHIFT, Key::F3), AppCommand::FindPrevious),
        (key(CMD, Key::N), AppCommand::NewTab),
        (key(CMD, Key::T), AppCommand::NewTab),
        (key(CMD, Key::O), AppCommand::OpenDialog),
        (key(CMD, Key::P), AppCommand::QuickOpen),
        (key(Modifiers::NONE, Key::F11), AppCommand::ToggleFocusMode),
        (key(CMD, Key::S), AppCommand::Save),
        (key(CMD, Key::W), AppCommand::CloseActiveTab),
        (key(CMD, Key::Tab), AppCommand::NextTab),
        (key(CMD, Key::PageDown), AppCommand::NextTab),
        (key(CMD, Key::PageUp), AppCommand::PrevTab),
        (key(CMD, Key::F), AppCommand::Find),
        (key(CMD, Key::G), AppCommand::GoToLine),
        (key(CMD, Key::Enter), AppCommand::CycleMode),
        (key(CMD, Key::Plus), AppCommand::ZoomIn),
        (key(CMD, Key::Equals), AppCommand::ZoomIn),
        (key(CMD, Key::Minus), AppCommand::ZoomOut),
        (key(CMD, Key::Num0), AppCommand::ZoomReset),
        (key(Modifiers::NONE, Key::F3), AppCommand::FindNext),
        (key(Modifiers::NONE, Key::F5), AppCommand::InsertTimeDate),
    ]
}

/// Shortcuts shown in menus but not bound here: the editor (egui's
/// `TextEdit`) already handles these keys itself while it has focus, and
/// consuming them would also break them in the find bar's text fields.
const fn editor_handled_shortcuts() -> [(KeyboardShortcut, AppCommand); 8] {
    let key = KeyboardShortcut::new;
    [
        (key(CMD, Key::Z), AppCommand::Undo),
        (key(CMD, Key::Y), AppCommand::Redo),
        (key(CMD, Key::X), AppCommand::Cut),
        (key(CMD, Key::C), AppCommand::Copy),
        (key(CMD, Key::V), AppCommand::Paste),
        (key(Modifiers::NONE, Key::Delete), AppCommand::Delete),
        (key(CMD, Key::A), AppCommand::SelectAll),
        // Ctrl+Shift+C reaches egui as a copy, not a key; see
        // `handle_keyboard_shortcuts`.
        (key(CMD_SHIFT, Key::C), AppCommand::CopyFormatted),
    ]
}

impl AppCommand {
    /// The primary key binding of this command, for display in menus: the
    /// first one in table order (core table first).
    pub(crate) fn shortcut(&self) -> Option<KeyboardShortcut> {
        bound_shortcuts()
            .into_iter()
            .chain(editor_handled_shortcuts())
            .find_map(|(shortcut, cmd)| (cmd == *self).then_some(shortcut))
    }
}

impl RustdownApp {
    /// Consume the bound shortcuts pressed this frame, in table order.
    /// Consuming keeps them away from the editor (Ctrl+Tab would insert a
    /// tab character) and from egui's built-in keyboard zoom (which would
    /// apply a second zoom step).
    pub(crate) fn consume_shortcuts(ctx: &egui::Context) -> Vec<AppCommand> {
        ctx.input_mut(|input| {
            shortcut_table()
                .into_iter()
                .filter_map(|(shortcut, cmd)| input.consume_shortcut(&shortcut).then_some(cmd))
                .collect()
        })
    }

    /// Run one command against the app / active tab.
    pub(crate) fn execute(&mut self, cmd: AppCommand, ctx: &egui::Context) {
        match cmd {
            AppCommand::NewTab => self.new_tab(),
            AppCommand::OpenDialog => self.open_file_dialog(),
            AppCommand::OpenPath(path) => self.open_path(path),
            AppCommand::Save => {
                if self.tab_mut().save_doc(false) {
                    self.note_saved_file();
                }
            }
            AppCommand::SaveAs => {
                if self.tab_mut().save_doc(true) {
                    self.note_saved_file();
                }
            }
            AppCommand::SaveAll => self.save_all(),
            AppCommand::Print => self.print_document(ctx),
            AppCommand::ExportPdf => self.export_pdf(ctx),
            AppCommand::CloseActiveTab => self.request_close_tab(self.tab().id),
            AppCommand::CloseTab(id) => self.request_close_tab(id),
            AppCommand::Exit => self.request_exit(),
            AppCommand::NextTab => self.cycle_tab(true),
            AppCommand::PrevTab => self.cycle_tab(false),
            AppCommand::ActivateTab(id) => {
                if let Some(index) = self.tab_index(id) {
                    self.activate_tab(index);
                }
            }
            AppCommand::Undo
            | AppCommand::Redo
            | AppCommand::Cut
            | AppCommand::Copy
            | AppCommand::Delete
            | AppCommand::SelectAll
            | AppCommand::InsertTimeDate => {
                if let Some(event) = edit_ops::editor_event_for(&cmd) {
                    self.queue_editor_event(ctx, event);
                }
            }
            AppCommand::Paste => self.paste_into_editor(ctx),
            AppCommand::Find => self.open_search(false),
            AppCommand::FindNext => self.find_in_editor(ctx, true),
            AppCommand::FindPrevious => self.find_in_editor(ctx, false),
            AppCommand::Replace => self.open_search(true),
            AppCommand::CloseSearch => self.close_search(),
            AppCommand::GoToLine => self.open_goto_dialog(ctx),
            AppCommand::FormatDocument => self.tab_mut().format_document(),
            AppCommand::SetMode(mode) => self.set_mode(mode, ctx),
            AppCommand::CycleMode => self.set_mode(self.mode.cycle(), ctx),
            AppCommand::ToggleOutline => {
                self.nav_visible = !self.nav_visible;
                self.save_preferences();
            }
            AppCommand::ZoomIn => self.adjust_zoom(ZOOM_STEP),
            AppCommand::ZoomOut => self.adjust_zoom(-ZOOM_STEP),
            AppCommand::ZoomReset => self.reset_zoom(),
            AppCommand::ToggleHeadingColors => {
                self.heading_color_mode = !self.heading_color_mode;
                for tab in &mut self.tabs {
                    tab.doc.editor_galley_cache = None;
                    tab.doc.preview_cache.clear();
                }
                self.save_preferences();
            }
            AppCommand::ToggleLinkedScrolling => {
                self.side_by_side_scroll_sync = !self.side_by_side_scroll_sync;
                self.tab_mut().clear_side_by_side_scroll_state();
                self.save_preferences();
            }
            AppCommand::ToggleStatusBar => {
                self.status_bar_visible = !self.status_bar_visible;
                self.save_preferences();
            }
            AppCommand::ToggleWordWrap => {
                self.word_wrap = !self.word_wrap;
                self.save_preferences();
            }
            AppCommand::ToggleSpellCheck => self.toggle_spell_check(ctx),
            AppCommand::ToggleFocusMode => self.toggle_focus_mode(ctx),
            AppCommand::CopyFormatted => self.copy_formatted(ctx),
            AppCommand::PasteImage => self.paste_image(ctx, true),
            AppCommand::ClearRecentFiles => {
                self.recent_files.clear();
                self.save_preferences();
            }
            AppCommand::MoveTab { id, to } => self.move_tab(id, to),
            AppCommand::QuickOpen => self.open_quick_open(),
            AppCommand::ToggleCloseWindowWithLastTab => {
                self.close_window_with_last_tab = !self.close_window_with_last_tab;
                self.save_preferences();
            }
            AppCommand::ToggleFormatToolbar => {
                self.show_format_toolbar = !self.show_format_toolbar;
                self.save_preferences();
            }
            AppCommand::SetTheme(theme) => {
                self.theme = theme;
                self.apply_theme(ctx);
                self.save_preferences();
            }
            AppCommand::MinimizeWindow => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            AppCommand::ToggleMaximizeWindow => {
                let maximized = ctx.input(|input| input.viewport().maximized);
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(
                    !maximized.unwrap_or(false),
                ));
            }
            AppCommand::OpenBundled(bundled) => self.open_bundled(bundled),
            AppCommand::ShowAbout => {
                self.show_about = true;
                self.legal_text = None;
            }
            AppCommand::Format(cmd) => self.execute_format(cmd, ctx),
            AppCommand::Explorer(cmd) => self.execute_explorer(cmd, ctx),
            AppCommand::Windows(cmd) => self.execute_windows(cmd, ctx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, modifiers: Modifiers) -> egui::RawInput {
        egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        }
    }

    fn consumed(input: egui::RawInput) -> Vec<AppCommand> {
        let ctx = egui::Context::default();
        let mut cmds = Vec::new();
        let _ = ctx.run(input, |ctx| cmds = RustdownApp::consume_shortcuts(ctx));
        cmds
    }

    #[test]
    fn menus_show_notepad_shortcuts() {
        let shortcut = |modifiers, key| Some(KeyboardShortcut::new(modifiers, key));
        for (cmd, expected) in [
            (AppCommand::Undo, shortcut(CMD, Key::Z)),
            (AppCommand::Redo, shortcut(CMD, Key::Y)),
            (AppCommand::Cut, shortcut(CMD, Key::X)),
            (AppCommand::Copy, shortcut(CMD, Key::C)),
            (AppCommand::Paste, shortcut(CMD, Key::V)),
            (AppCommand::Delete, shortcut(Modifiers::NONE, Key::Delete)),
            (AppCommand::SelectAll, shortcut(CMD, Key::A)),
            (AppCommand::Find, shortcut(CMD, Key::F)),
            (AppCommand::FindNext, shortcut(Modifiers::NONE, Key::F3)),
            (
                AppCommand::FindPrevious,
                shortcut(Modifiers::SHIFT, Key::F3),
            ),
            (AppCommand::Replace, shortcut(CMD, Key::H)),
            (AppCommand::GoToLine, shortcut(CMD, Key::G)),
            (
                AppCommand::InsertTimeDate,
                shortcut(Modifiers::NONE, Key::F5),
            ),
            (AppCommand::SaveAll, shortcut(CMD_ALT, Key::S)),
        ] {
            assert_eq!(cmd.shortcut(), expected, "{cmd:?}");
        }
    }

    #[test]
    fn notepad_keys_dispatch_commands() {
        for (key, modifiers, expected) in [
            (Key::F3, Modifiers::NONE, AppCommand::FindNext),
            (Key::F3, Modifiers::SHIFT, AppCommand::FindPrevious),
            (Key::H, CMD, AppCommand::Replace),
            (Key::F, CMD_SHIFT, AppCommand::Replace),
            (Key::G, CMD, AppCommand::GoToLine),
            (Key::F5, Modifiers::NONE, AppCommand::InsertTimeDate),
            (Key::S, CMD_ALT, AppCommand::SaveAll),
            (Key::S, CMD_SHIFT, AppCommand::SaveAs),
            (Key::S, CMD, AppCommand::Save),
        ] {
            assert_eq!(
                consumed(press(key, modifiers)),
                [expected],
                "{key:?} {modifiers:?}"
            );
        }
    }

    #[test]
    fn editor_keys_are_left_to_the_editor() {
        // Binding these would steal them from the editor and the find bar.
        for (key, modifiers) in [
            (Key::Z, CMD),
            (Key::Y, CMD),
            (Key::X, CMD),
            (Key::C, CMD),
            (Key::V, CMD),
            (Key::A, CMD),
            (Key::Delete, Modifiers::NONE),
        ] {
            assert!(consumed(press(key, modifiers)).is_empty(), "{key:?}");
            assert!(
                !shortcut_table()
                    .iter()
                    .any(|(shortcut, _)| shortcut.logical_key == key
                        && shortcut.modifiers == modifiers),
                "{key:?} must not be bound"
            );
        }
    }

    #[test]
    fn formatting_keys_stay_free() {
        for (key, modifiers) in [
            (Key::B, CMD),
            (Key::I, CMD),
            (Key::K, CMD),
            (Key::E, CMD),
            (Key::Num1, CMD),
            (Key::Num6, CMD),
            (Key::B, CMD_SHIFT),
            (Key::X, CMD_SHIFT),
        ] {
            assert!(
                !core_shortcuts()
                    .iter()
                    .any(|(shortcut, _)| shortcut.logical_key == key
                        && shortcut.modifiers == modifiers),
                "{key:?} {modifiers:?} is reserved for formatting"
            );
        }
    }

    #[test]
    fn no_two_commands_share_a_key_combination() {
        let table = shortcut_table();
        for (i, (a, cmd_a)) in table.iter().enumerate() {
            for (b, cmd_b) in &table[i + 1..] {
                assert!(
                    a.modifiers != b.modifiers || a.logical_key != b.logical_key || cmd_a == cmd_b,
                    "{a:?} is bound to both {cmd_a:?} and {cmd_b:?}"
                );
            }
        }
    }
}
