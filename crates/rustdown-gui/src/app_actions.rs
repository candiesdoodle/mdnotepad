use std::path::{Path, PathBuf};

use eframe::egui;

use super::{
    BundledDoc, CloseChoice, ClosePrompt, Mode, RustdownApp, clamped_zoom_factor,
    markdown_file_dialog, zoom_with_factor, zoom_with_step,
};
use crate::{
    cli::LaunchOptions,
    live_format, nav, preferences,
    tab::{Tab, TabId, canonical_path},
    ui_style,
};

/// Window title suffix, Notepad style: `*notes.md - <app name>`.
pub(crate) const TITLE_SUFFIX: &str = concat!(" - ", crate::app_identity::app_name!());

impl RustdownApp {
    pub(crate) fn from_launch_options(options: LaunchOptions) -> Self {
        Self::from_launch_options_and_preferences(options, &preferences::UserPreferences::load())
    }

    /// The app for `options`, with the settings in `prefs`.
    pub(crate) fn from_launch_options_and_preferences(
        mut options: LaunchOptions,
        prefs: &preferences::UserPreferences,
    ) -> Self {
        // A folder argument opens it in the Explorer instead of a tab.
        let folder = options.path.take_if(|path| path.is_dir());

        // A mode flag on the command line wins, and a plain text file opens
        // as plain text (Raw); otherwise Settings → Start in decides, with
        // or without a file.
        let chosen_mode = Mode::from_str_lossy(&prefs.mode).unwrap_or_default();
        let start_mode = Mode::from_str_lossy(&prefs.start_mode);
        let text_file = options.path.as_deref().is_some_and(crate::is_text_path);
        let mode = if options.mode_explicit || text_file {
            options.mode
        } else {
            start_mode.unwrap_or(chosen_mode)
        };

        let mut app = Self {
            mode,
            chosen_mode,
            start_mode,
            nav_visible: prefs.nav_visible,
            heading_color_mode: prefs.heading_color_mode,
            side_by_side_scroll_sync: prefs.side_by_side_scroll_sync,
            auto_save: prefs.auto_save,
            theme: ui_style::ThemeChoice::from_str_lossy(&prefs.theme).unwrap_or_default(),
            word_wrap: prefs.word_wrap,
            status_bar_visible: prefs.show_status_bar,
            show_format_toolbar: prefs.show_format_toolbar,
            close_window_with_last_tab: prefs.close_window_with_last_tab,
            window: crate::window_state::WindowState::from_preferences(prefs),
            recent_files: prefs.recent_files.iter().map(PathBuf::from).collect(),
            zoom: clamped_zoom_factor(prefs.zoom_factor),
            ..Self::default()
        };
        app.spell.enabled = prefs.spell_check;
        app.restore_explorer(&prefs.explorer, folder.as_deref());
        if let Some(path) = options.path {
            app.open_path(path);
        }
        // Auto-show nav in preview modes if heading count exceeds threshold.
        app.maybe_auto_show_nav();
        app
    }

    // ── Tabs ────────────────────────────────────────────────────────────

    /// The active tab.
    pub(crate) fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// The active tab, mutably.
    pub(crate) fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub(crate) fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Make the tab at `index` active.
    pub(crate) fn activate_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        self.active = index;
        self.on_active_tab_changed();
    }

    pub(crate) fn on_active_tab_changed(&mut self) {
        // `edit_seq` is per document, so a cached count may belong to another tab.
        self.search.invalidate_match_cache();
        self.scroll_to_active_tab = true;
        self.focus_editor = true;
    }

    /// Activate the next (`forward`) or previous tab, wrapping around.
    pub(crate) fn cycle_tab(&mut self, forward: bool) {
        let count = self.tabs.len();
        let next = if forward {
            (self.active + 1) % count
        } else {
            (self.active + count - 1) % count
        };
        self.activate_tab(next);
    }

    /// Append an empty Untitled tab and activate it.
    pub(crate) fn new_tab(&mut self) {
        let mut tab = Tab::default();
        tab.doc.line_ending = self.untitled_line_ending;
        self.tabs.push(tab);
        self.activate_tab(self.tabs.len() - 1);
    }

    /// Add `tab` and activate it.  An untouched Untitled active tab is
    /// replaced instead of kept next to it (as Notepad does).
    pub(crate) fn add_tab(&mut self, tab: Tab) {
        if self.tab().is_untouched() {
            self.tabs[self.active] = tab;
            self.on_active_tab_changed();
        } else {
            self.tabs.push(tab);
            self.activate_tab(self.tabs.len() - 1);
        }
    }

    /// Open `path` in a tab.  Focuses the tab that already shows that file
    /// (compared by canonical path), otherwise loads it into a new tab.
    /// Every "open a file" entry point (dialog, drag & drop, CLI, explorer)
    /// goes through here.
    pub(crate) fn open_path(&mut self, path: PathBuf) {
        let canonical = canonical_path(&path);
        if let Some(index) = self.tabs.iter().position(|tab| tab.has_path(&canonical)) {
            self.activate_tab(index);
            return;
        }
        match Tab::from_path(path) {
            Ok(tab) => {
                self.note_recent_file(&canonical);
                self.add_tab(tab);
            }
            Err(err) => {
                self.tab_mut()
                    .error
                    .get_or_insert_with(|| format!("Open failed: {err}"));
            }
        }
    }

    /// Most files kept in File → Recent files.
    const RECENT_FILES: usize = 10;

    /// Put `path` first in File → Recent files (and in Windows' recent
    /// documents: the taskbar icon's Jump List).
    pub(crate) fn note_recent_file(&mut self, path: &Path) {
        self.recent_files
            .retain(|known| !same_file_name(known, path));
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(Self::RECENT_FILES);
        #[cfg(windows)]
        if !cfg!(test) {
            crate::win_ffi::add_to_recent_documents(path);
        }
        self.save_preferences();
    }

    /// After a save: the active tab's file goes first in Recent files
    /// (a new file gets there this way).
    pub(crate) fn note_saved_file(&mut self) {
        if let Some(path) = self.tab().doc.path.clone() {
            self.note_recent_file(&canonical_path(&path));
        }
    }

    /// Drag a tab: move tab `id` to position `to` (the active tab stays
    /// active).
    pub(crate) fn move_tab(&mut self, id: TabId, to: usize) {
        let Some(from) = self.tab_index(id) else {
            return;
        };
        let to = to.min(self.tabs.len() - 1);
        if from == to {
            return;
        }
        let active = self.tab().id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.tab_index(active).unwrap_or(to);
    }

    pub(crate) fn open_file_dialog(&mut self) {
        if let Some(path) = markdown_file_dialog().pick_file() {
            self.open_path(path);
        }
    }

    pub(crate) fn open_bundled(&mut self, bundled: BundledDoc) {
        self.add_tab(Tab::from_bundled(bundled));
    }

    /// Close tab `id`, asking "Save changes?" first if it is dirty.
    pub(crate) fn request_close_tab(&mut self, id: TabId) {
        let Some(index) = self.tab_index(id) else {
            return;
        };
        if self.tabs[index].doc.dirty {
            self.activate_tab(index);
            self.close_prompt = Some(ClosePrompt {
                queue: vec![id],
                closing_window: false,
            });
        } else {
            self.close_tab_now(index);
        }
    }

    /// Remove the tab at `index` without asking.  Closing the last tab
    /// closes the window instead (Notepad behaviour) or, with Settings →
    /// Close window with last tab off, leaves an empty Untitled tab, so
    /// `tabs` is never empty.
    pub(crate) fn close_tab_now(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            if self.close_window_with_last_tab {
                self.window_close_pending = true;
            } else {
                self.new_tab();
                self.tabs.remove(0);
                self.active = 0;
                self.on_active_tab_changed();
            }
            return;
        }
        self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.on_active_tab_changed();
    }

    // ── Closing: prompts and window close ───────────────────────────────

    /// `true` while a modal prompt (close prompt, Go to line, or the active
    /// tab's question about its changed file) is waiting for an answer; keyboard shortcuts are
    /// ignored.
    pub(crate) fn dialog_open(&self) -> bool {
        self.close_prompt.is_some()
            || self.goto_dialog.is_some()
            || self.quick_open.is_some()
            || self.pending_image.is_some()
            || self.confirm_reset
            || self.tab().disk.awaits_answer()
    }

    /// Start closing the window.  Returns `true` when nothing is unsaved and
    /// the window may close now; otherwise queues a prompt for every dirty
    /// tab (activating the first) and returns `false`.
    pub(crate) fn begin_window_close(&mut self) -> bool {
        if self.keeps_session() {
            // Unsaved text is kept for the next start, so nothing to ask.
            self.save_session();
            return true;
        }
        let queue: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|tab| tab.doc.dirty)
            .map(|tab| tab.id)
            .collect();
        let Some(&first) = queue.first() else {
            return true;
        };
        if let Some(index) = self.tab_index(first) {
            self.activate_tab(index);
        }
        self.close_prompt = Some(ClosePrompt {
            queue,
            closing_window: true,
        });
        false
    }

    /// File → Exit: close the window, prompting for unsaved tabs first.
    pub(crate) fn request_exit(&mut self) {
        if self.begin_window_close() {
            self.window_close_pending = true;
        }
    }

    /// Intercept the OS close button / Alt+F4 so unsaved tabs are not lost.
    pub(crate) fn handle_close_request(&mut self, ctx: &egui::Context) {
        if self.allow_window_close || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if self.begin_window_close() {
            self.allow_window_close = true;
            crate::startup_timing::close_started();
            // The window's place and size, and the tabs, for next time.
            self.save_preferences();
            self.save_session();
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
    }

    /// Send the deferred window close (last tab closed, Exit, or prompts done).
    pub(crate) fn flush_window_close(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.window_close_pending) {
            self.allow_window_close = true;
            crate::startup_timing::close_started();
            self.save_preferences();
            self.save_session();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Apply the user's answer for the tab at the front of the close prompt.
    ///
    /// - Save: saves (Save As for Untitled); a cancelled dialog, failed write
    ///   or a question about the changed file cancels the whole close.
    /// - Don't save: discards that tab's changes.
    /// - Cancel: aborts; nothing is closed.
    ///
    /// For a single-tab close the tab is then closed.  For a window close
    /// tabs stay open while the queue is worked through (so Cancel part-way
    /// loses nothing) and the window closes once the queue is empty.
    pub(crate) fn resolve_close_prompt(&mut self, choice: CloseChoice) {
        let Some(ClosePrompt {
            mut queue,
            closing_window,
        }) = self.close_prompt.take()
        else {
            return;
        };
        if choice == CloseChoice::Cancel || queue.is_empty() {
            return;
        }

        let id = queue.remove(0);
        if let Some(index) = self.tab_index(id) {
            if choice == CloseChoice::Save {
                self.activate_tab(index);
                if !self.tabs[index].save_doc(false) {
                    return;
                }
            }
            if !closing_window {
                self.close_tab_now(index);
            }
        }

        // Drop tabs that were closed or saved in the meantime.
        queue.retain(|id| {
            self.tab_index(*id)
                .is_some_and(|index| self.tabs[index].doc.dirty)
        });
        if let Some(&next) = queue.first() {
            if let Some(index) = self.tab_index(next) {
                self.activate_tab(index);
            }
            self.close_prompt = Some(ClosePrompt {
                queue,
                closing_window,
            });
        } else if closing_window {
            self.window_close_pending = true;
        }
    }

    // ── Mode, preferences, zoom ─────────────────────────────────────────

    pub(crate) fn set_mode(&mut self, mode: Mode, ctx: &egui::Context) {
        let newly_chosen = self.chosen_mode != mode;
        self.chosen_mode = mode;
        if self.mode == mode {
            if newly_chosen {
                self.save_preferences();
            }
            return;
        }

        // Capture the current scroll position as a byte offset before
        // switching modes, so the new mode can jump to the same location.
        let old_mode = self.mode;
        let scroll_byte = self.tab_mut().current_scroll_byte_offset(ctx, old_mode);

        self.mode = mode;
        for tab in &mut self.tabs {
            tab.clear_side_by_side_scroll_state();
            if mode == Mode::Preview {
                // Preview doesn't render the editor; drop any cached galley to reduce memory.
                tab.doc.clear_editor_caches();
            }
            if mode != Mode::Edit {
                // Only Edit lays the text out formatted.
                tab.doc.formatted_analysis = None;
            }
            if matches!(mode, Mode::Edit | Mode::Raw) {
                // No preview in these modes; drop its cache to reduce memory.
                tab.doc.preview_cache.clear();
                tab.doc.preview_dirty = false;
                tab.doc.last_edit_at = None;
            }
        }

        // Auto-show nav in preview modes if heading count exceeds threshold.
        self.maybe_auto_show_nav();

        // Request a scroll to the same position in the new mode.
        if let Some(byte_offset) = scroll_byte {
            self.tab_mut().nav.pending_scroll =
                Some(nav::panel::NavScrollTarget::ByteOffset(byte_offset));
        }

        self.save_preferences();
    }

    /// Whether the active tab's editor shows the formatted view: Edit mode,
    /// and the document is not too large for it.  Otherwise (Raw, Split,
    /// or a large file in Edit) the editor shows the raw text.
    pub(crate) fn formatted_view(&self) -> bool {
        self.mode == Mode::Edit && !self.formatted_view_too_large()
    }

    /// The active document is over [`live_format::MAX_FORMATTED_BYTES`]:
    /// Edit shows it as raw text.
    pub(crate) fn formatted_view_too_large(&self) -> bool {
        self.tab().doc.text.len() > live_format::MAX_FORMATTED_BYTES
    }

    /// Auto-show the nav panel in Preview/SideBySide when the document has
    /// enough headings to benefit from a table of contents.
    pub(crate) fn maybe_auto_show_nav(&mut self) {
        if matches!(self.mode, Mode::Preview | Mode::SideBySide)
            && self.tab().nav.outline.len() >= preferences::AUTO_NAV_MIN_HEADINGS
        {
            self.nav_visible = true;
        }
    }

    /// Persist the current preferences to disk.
    pub(crate) fn save_preferences(&self) {
        // Unit tests must not overwrite the developer's real settings file.
        if cfg!(test) {
            return;
        }
        self.preferences().save();
    }

    /// The current settings, as persisted.
    pub(crate) fn preferences(&self) -> preferences::UserPreferences {
        preferences::UserPreferences {
            nav_visible: self.nav_visible,
            heading_color_mode: self.heading_color_mode,
            side_by_side_scroll_sync: self.side_by_side_scroll_sync,
            zoom_factor: self.zoom,
            mode: self.chosen_mode.as_str().to_owned(),
            start_mode: self.start_mode.map_or("last", Mode::as_str).to_owned(),
            auto_save: self.auto_save,
            theme: self.theme.as_str().to_owned(),
            word_wrap: self.word_wrap,
            spell_check: self.spell.enabled,
            show_status_bar: self.status_bar_visible,
            show_format_toolbar: self.show_format_toolbar,
            close_window_with_last_tab: self.close_window_with_last_tab,
            recent_files: self
                .recent_files
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            window_position: self.window.position,
            window_size: self.window.size,
            window_maximized: self.window.maximized,
            explorer: self.explorer_preferences(),
            markdown_syntax: false,
        }
    }

    // Zoom scales the whole window: it is egui's zoom factor, applied by
    // `sync_zoom` each frame.  Sizes in points stay the same.

    /// Give egui the zoom (it takes effect from the next frame).
    pub(crate) fn sync_zoom(&self, ctx: &egui::Context) {
        if ctx.zoom_factor().to_bits() != self.zoom.to_bits() {
            ctx.set_zoom_factor(self.zoom);
        }
    }

    /// Zoom in or out by `delta` (Ctrl+Plus / Ctrl+Minus).
    pub(crate) fn adjust_zoom(&mut self, delta: f32) {
        self.set_zoom(zoom_with_step(self.zoom, delta));
    }

    /// Zoom by `factor` (Ctrl+wheel, pinch).
    pub(crate) fn adjust_zoom_factor(&mut self, factor: f32) {
        self.set_zoom(zoom_with_factor(self.zoom, factor));
    }

    /// Back to 100 % (Ctrl+0).
    pub(crate) fn reset_zoom(&mut self) {
        self.set_zoom(1.0);
    }

    /// Zoom to `zoom` (clamped to the zoom range), and save it.
    pub(crate) fn set_zoom(&mut self, zoom: f32) {
        let zoom = clamped_zoom_factor(zoom);
        if zoom.to_bits() != self.zoom.to_bits() {
            self.zoom = zoom;
            self.save_preferences();
        }
    }

    /// Notepad-style window title: `<tab title> - <app name>`, prefixed with
    /// `*` while the active tab has unsaved changes.
    pub(crate) fn update_viewport_title(&mut self, ctx: &egui::Context) {
        let tab = &self.tabs[self.active];
        let title = tab.title();
        let dirty_mark = if tab.doc.dirty { "*" } else { "" };

        // Avoid allocating when nothing changed.
        let last = &self.last_viewport_title;
        if last.len() == dirty_mark.len() + title.len() + TITLE_SUFFIX.len()
            && last.starts_with(dirty_mark)
            && last.ends_with(TITLE_SUFFIX)
            && last.get(dirty_mark.len()..dirty_mark.len() + title.len()) == Some(title.as_ref())
        {
            return;
        }

        self.last_viewport_title.clear();
        self.last_viewport_title.push_str(dirty_mark);
        self.last_viewport_title.push_str(&title);
        self.last_viewport_title.push_str(TITLE_SUFFIX);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            self.last_viewport_title.clone(),
        ));
    }

    /// Auto-save every dirty, file-backed tab (not just the active one).
    pub(crate) fn tick_auto_save(&mut self, ctx: &egui::Context) {
        let enabled = self.auto_save;
        for tab in &mut self.tabs {
            tab.tick_auto_save(ctx, enabled);
        }
    }

    // ── Search ──────────────────────────────────────────────────────────

    pub(crate) const fn open_search(&mut self, replace_mode: bool) {
        self.search.visible = true;
        self.search.replace_mode = replace_mode;
        self.search.last_replace_count = None;
        self.focus_search = true;
    }

    pub(crate) const fn close_search(&mut self) {
        self.search.visible = false;
        self.search.replace_mode = false;
        self.search.last_replace_count = None;
        self.focus_search = false;
    }

    /// Match count of the search query in the active tab (cached).
    pub(crate) fn active_match_count(&mut self) -> usize {
        let doc = &self.tabs[self.active].doc;
        self.search.match_count(doc.text.as_str(), doc.edit_seq)
    }

    /// Replace every match of the search query in the active tab.
    pub(crate) fn replace_all_matches(&mut self) -> usize {
        let tab = &mut self.tabs[self.active];
        tab.replace_all_matches(&self.search.query, &self.search.replacement)
    }
}

/// Whether `a` and `b` name the same file (ignoring case on Windows, as
/// its file system does).
fn same_file_name(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}
