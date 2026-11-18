use std::{cell::Cell, sync::Arc};

use crate::md::{MarkdownStyle, MarkdownViewer};
use eframe::egui;

use super::{
    AppCommand, CloseChoice, DEBOUNCE, Mode, PANEL_EDGE_PADDING, RustdownApp,
    SCROLL_WHEEL_MULTIPLIER, markdown_paths,
};
use crate::{
    disk::sync::DiskChangeChoice,
    document::{self, Document, EditorView, TrackedTextBuffer},
    edit_ops, editor, live_format,
    tab::Tab,
    ui_style,
};
use eframe::egui::text::{CCursor, CCursorRange};

/// Whether `reveal` already shows the marks of all of `lines`.
const fn reveal_covers(reveal: live_format::Reveal, lines: live_format::Reveal) -> bool {
    match (reveal, lines) {
        (Some((first, last)), Some((a, b))) => first <= a && b <= last,
        _ => false,
    }
}

/// Make the formatted view's task checkboxes clickable; returns the byte
/// to toggle when one was clicked.
fn task_checkbox_clicked(
    ui: &egui::Ui,
    editor_id: egui::Id,
    overlays: &[live_format::Overlay],
    origin: egui::Pos2,
    style: &live_format::FormatStyle,
) -> Option<usize> {
    let clip = ui.clip_rect();
    let mut clicked = None;
    for overlay in overlays {
        let live_format::OverlayKind::Checkbox { toggle_byte, .. } = overlay.kind else {
            continue;
        };
        let rect = live_format::checkbox_square(overlay.rect.translate(origin.to_vec2()), style)
            .expand(2.0);
        if !rect.intersects(clip) {
            continue;
        }
        // Senses drags too, so a press on it does not reach the editor
        // below (which would move the caret there).
        let response = ui
            .interact(
                rect,
                editor_id.with(("task", toggle_byte)),
                egui::Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            clicked = Some(toggle_byte);
        }
    }
    clicked
}

impl RustdownApp {
    /// Read keyboard/mouse input and dispatch the matching commands (open,
    /// save, tabs, zoom, search, format, mode-cycle, etc.).  Skipped when a
    /// dialog is open so that shortcuts do not fire behind modal windows.
    pub(crate) fn handle_keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        let dialog_open = self.dialog_open();

        let dropped = ctx.input(|i| {
            markdown_paths(
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|file| file.path.as_deref()),
            )
        });
        let shortcut_cmds = Self::consume_shortcuts(ctx);
        // Ctrl+Shift+C reaches egui as a copy (not a key): taken from the
        // editor, it copies as formatted text instead.
        let formatted_copy = ctx.input_mut(|input| {
            if !(input.modifiers.command && input.modifiers.shift) {
                return false;
            }
            let before = input.events.len();
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Copy));
            input.events.len() != before
        });
        let (zoom_delta, escape) =
            ctx.input(|i| (i.zoom_delta(), i.key_pressed(egui::Key::Escape)));

        if dialog_open {
            return;
        }
        let mut cmds: Vec<AppCommand> = dropped.into_iter().map(AppCommand::OpenPath).collect();
        cmds.extend(shortcut_cmds);
        if formatted_copy {
            cmds.push(AppCommand::CopyFormatted);
        }
        if escape && self.search.visible {
            cmds.push(AppCommand::CloseSearch);
        } else if escape && self.focus_mode {
            cmds.push(AppCommand::ToggleFocusMode);
        }
        for cmd in cmds {
            self.execute(cmd, ctx);
        }
        // Ctrl+scroll / pinch zoom is continuous, so it is not a command.
        if (zoom_delta - 1.0).abs() > f32::EPSILON {
            self.adjust_zoom_factor(zoom_delta);
        }
    }

    /// Render the search/replace bar and execute a pending replace-all after
    /// the panel closure finishes (so the `&mut self` borrow is released).
    pub(crate) fn show_search_bar(&mut self, ctx: &egui::Context) {
        let mut run_replace_all = false;
        let mut find = None;
        egui::TopBottomPanel::bottom("search").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Find:");
                let query_response = ui.add(
                    egui::TextEdit::singleline(&mut self.search.query)
                        .hint_text("Search")
                        .desired_width(180.0)
                        .id(egui::Id::new(edit_ops::FIND_QUERY_ID)),
                );
                if self.focus_search {
                    query_response.request_focus();
                    self.focus_search = false;
                }
                if query_response.changed() {
                    self.search.last_replace_count = None;
                }
                if query_response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    find = Some(AppCommand::FindNext);
                }

                let matches = self.active_match_count();
                let label = if matches == 1 { "match" } else { "matches" };
                ui.label(format!("{matches} {label}"));
                let can_find = self.mode.uses_editor() && matches > 0;
                for (text, hover, cmd) in [
                    ("↑", "Find previous (Shift+F3)", AppCommand::FindPrevious),
                    ("↓", "Find next (F3)", AppCommand::FindNext),
                ] {
                    if ui
                        .add_enabled(can_find, egui::Button::new(text))
                        .on_hover_text(hover)
                        .clicked()
                    {
                        find = Some(cmd);
                    }
                }

                if self.search.replace_mode {
                    ui.separator();
                    ui.label("Replace:");
                    let replace_response = ui.add(
                        egui::TextEdit::singleline(&mut self.search.replacement)
                            .hint_text("Replace with")
                            .desired_width(180.0),
                    );
                    if replace_response.changed() {
                        self.search.last_replace_count = None;
                    }

                    let replace_button = ui.add_enabled(
                        !self.search.query.is_empty(),
                        egui::Button::new("Replace all"),
                    );
                    if replace_button.clicked() {
                        run_replace_all = true;
                    }

                    if let Some(count) = self.search.last_replace_count {
                        ui.label(format!("replaced {count}"));
                    }
                }

                if ui.button("Close").clicked() {
                    self.close_search();
                }
            });
        });
        if run_replace_all {
            let replaced = self.replace_all_matches();
            self.search.last_replace_count = Some(replaced);
        }
        if let Some(cmd) = find {
            // Like Notepad's Find dialog: stay in the find bar, so Enter
            // again finds the next match (the match shows as an inactive
            // selection in the editor).
            self.execute(cmd, ctx);
            self.focus_editor = false;
            self.focus_search = true;
        }
    }

    /// Render the nav panel, side-by-side preview panel, central panel, and
    /// process any pending nav-scroll actions.  Extracted so the debug harness
    /// can reuse the same layout sequence.
    #[allow(clippy::cast_possible_truncation)] // PANEL_EDGE_PADDING=8.0, fits in i8
    /// Opacity of the document after a switch of mode or tab: it fades in
    /// from `SWITCH_FADE_FROM` over `SWITCH_FADE_TIME`, so the change
    /// reads as one soft step instead of a jump.
    fn content_opacity(&mut self, ctx: &egui::Context) -> f32 {
        /// Opacity right after the switch.
        const SWITCH_FADE_FROM: f32 = 0.25;
        /// Length of the fade, in seconds.
        const SWITCH_FADE_TIME: f32 = 0.16;

        let now = ctx.input(|input| input.time);
        let shown = (self.mode, self.tab().id);
        if self
            .last_shown
            .replace(shown)
            .is_some_and(|was| was != shown)
        {
            self.switched_at = Some(now);
        }
        let Some(switched_at) = self.switched_at.filter(|_| ui_style::motion()) else {
            return 1.0;
        };
        let t = (now - switched_at) as f32 / SWITCH_FADE_TIME;
        if t >= 1.0 {
            self.switched_at = None;
            return 1.0;
        }
        ctx.request_repaint();
        egui::emath::easing::cubic_out(t.max(0.0)).mul_add(1.0 - SWITCH_FADE_FROM, SWITCH_FADE_FROM)
    }

    /// The central panel's view for `mode`.
    fn show_document(&mut self, ui: &mut egui::Ui, mode: Mode) {
        match mode {
            Mode::Edit | Mode::Raw | Mode::SideBySide => self.show_editor(ui),
            Mode::Preview => self.show_preview(ui),
        }
    }

    pub(crate) fn show_content_panels(&mut self, ctx: &egui::Context) {
        // Editor and preview sit on the page colour (white in the light theme).
        let panel_frame = egui::Frame::new()
            .fill(ui_style::content_background(&ctx.style().visuals))
            .inner_margin(PANEL_EDGE_PADDING as i8);
        let mode = self.mode;
        let opacity = self.content_opacity(ctx);
        // Focus mode shows only the document.
        let nav_visible = self.nav_visible && !self.focus_mode;
        let focus_mode = self.focus_mode;
        let heading_color_mode = self.heading_color_mode;
        let linked_scrolling = self.side_by_side_scroll_sync;

        {
            let tab = self.tab_mut();
            // Resolve any pending nav target to a y-pixel value *before* the
            // scroll areas render, so the smooth-scroll request is consumed on
            // this same frame (no 1-frame delay).
            tab.resolve_nav_scroll_target(ctx, mode);

            tab.nav.visible = nav_visible;
            if nav_visible {
                tab.nav.heading_color_mode = heading_color_mode;
                tab.nav.refresh_outline(&tab.doc.text, tab.doc.edit_seq);
            }
            tab.nav.show(ctx);
        }
        // The outline's hide button.
        if std::mem::take(&mut self.tab_mut().nav.hide_requested) {
            self.execute(AppCommand::ToggleOutline, ctx);
        }

        // Split: the raw editor (central panel) and the preview on the right.
        if mode == Mode::SideBySide {
            egui::SidePanel::right("preview")
                .resizable(true)
                .min_width(240.0)
                .default_width(420.0)
                .frame(panel_frame)
                .show(ctx, |ui| {
                    ui.multiply_opacity(opacity);
                    self.show_preview(ui);
                });
        }

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                ui.multiply_opacity(opacity);
                if focus_mode {
                    // The text in a centred column.
                    let column = Self::focus_column(ui.max_rect());
                    ui.scope_builder(egui::UiBuilder::new().max_rect(column), |ui| {
                        self.show_document(ui, mode);
                    });
                } else {
                    self.show_document(ui, mode);
                }
            });

        let tab = self.tab_mut();
        // Sync the active heading highlight from the current scroll position.
        if nav_visible {
            tab.sync_nav_active_heading(ctx, mode);
        }

        // In SideBySide, sync the preview scroll to match the editor position.
        if mode == Mode::SideBySide && linked_scrolling {
            tab.sync_side_by_side_scroll(ctx, linked_scrolling);
            tab.animate_side_by_side_scroll(ctx);
        }
    }

    /// The editor, in `ui` (a document panel, at the base text sizes).
    pub(crate) fn show_editor(&mut self, ui: &mut egui::Ui) {
        ui_style::TextSizes::base().apply(ui.style_mut());
        let formatted = self.formatted_view();
        let mut params = editor::LayoutParams {
            seq: 0,
            view: EditorView::Raw,
            heading_color_mode: self.heading_color_mode,
            nav_visible: self.nav_visible,
            word_wrap: self.word_wrap,
        };
        let (editor_id, tab_id) = (self.tab().editor_id(), self.tab().id);
        let spell_active = self.spell.active();
        // Edit-menu commands queue input events for the editor; they need
        // it focused, which may take one more frame.
        let focus_later = self.inject_pending_editor_events(ui.ctx(), editor_id, tab_id);
        let focus_editor = std::mem::take(&mut self.focus_editor) || focus_later;
        let scroll_to_cursor = std::mem::take(&mut self.editor_ops.scroll_to_cursor);
        let cursor_before = egui::TextEdit::load_state(ui.ctx(), editor_id)
            .and_then(|state| state.cursor.char_range());
        // Lists continue themselves: Enter on a list line, before the
        // editor reads the key.
        // Tables take Tab, Shift+Tab and Enter (`table_edit`); lists
        // continue themselves on Enter.  Both before the editor reads keys.
        if ui.ctx().memory(|memory| memory.has_focus(editor_id))
            && let Some(range) = cursor_before
        {
            let text = self.tab().doc.text.clone();
            if !self.table_keys(ui, &text, range.primary.index)
                && range.primary.index == range.secondary.index
            {
                crate::list_continue::handle_enter(ui, &text, range.primary.index);
            }
        }
        let tab = self.tab_mut();
        // Formatted view: the lines holding the caret show their marks.
        let reveal = formatted.then(|| {
            cursor_before.and_then(|range| {
                tab.doc
                    .formatted_analysis()
                    .reveal_for(range.primary.index, range.secondary.index)
            })
        });
        params.view = reveal.map_or(EditorView::Raw, EditorView::Formatted);
        // Spell check: the last check's marks, moved onto this text.
        let squiggles = spell_active.then(|| {
            tab.spelling.refresh(&tab.doc.text, tab.doc.edit_seq);
            tab.doc.edit_seq
        });
        let (response, menu_char, changed, next_seq, task_toggle, discarded) = {
            let seq = Cell::new(tab.doc.edit_seq);
            let spelling = &tab.spelling;
            let Document {
                text,
                line_ending,
                editor_galley_cache,
                formatted_analysis,
                ..
            } = &mut tab.doc;

            let mut buffer = TrackedTextBuffer {
                text,
                seq: &seq,
                line_ending: *line_ending,
            };

            let editor_size = ui.available_size();
            let scroll_to = tab.nav.pending_editor_scroll_y.take();
            let mut scroll_area = egui::ScrollArea::both()
                .id_salt(("editor_scroll", tab_id))
                .auto_shrink([false; 2])
                .wheel_scroll_multiplier(egui::vec2(1.0, SCROLL_WHEEL_MULTIPLIER));
            if let Some(y) = scroll_to {
                scroll_area = scroll_area.vertical_scroll_offset(y);
            }
            let output = scroll_area.show(ui, |ui| {
                // Code-block backgrounds go under the text.
                let backdrop = ui.painter().add(egui::Shape::Noop);
                let mut layouter =
                    |ui: &egui::Ui, text_buf: &dyn egui::TextBuffer, wrap_width: f32| {
                        let params = editor::LayoutParams {
                            seq: seq.get(),
                            ..params
                        };
                        editor::editor_galley(
                            ui,
                            text_buf.as_str(),
                            wrap_width,
                            &params,
                            editor_galley_cache,
                            formatted_analysis,
                        )
                    };
                let editor = egui::TextEdit::multiline(&mut buffer)
                    .desired_width(f32::INFINITY)
                    .font(if formatted {
                        egui::TextStyle::Body
                    } else {
                        egui::TextStyle::Monospace
                    })
                    .frame(false)
                    .id(editor_id)
                    // Tab types a tab (Notepad) instead of moving the focus.
                    .lock_focus(true)
                    .layouter(&mut layouter);
                // Same layout as `ui.add_sized`, but keeps the `TextEditOutput`.
                let layout = egui::Layout::centered_and_justified(ui.layout().main_dir());
                let edit = ui
                    .allocate_ui_with_layout(editor_size, layout, |ui| editor.show(ui))
                    .inner;
                // Diagram pictures (Mermaid blocks off the caret).
                if let Some(cache) = editor_galley_cache.as_ref() {
                    for (rect, uri) in &cache.pictures {
                        let rect = rect.translate(edit.galley_pos.to_vec2());
                        if ui.is_rect_visible(rect) {
                            egui::Image::new(uri.as_str()).paint_at(ui, rect);
                        }
                    }
                }
                let task_toggle = editor_galley_cache.as_ref().and_then(|cache| {
                    let style = cache.format_style.as_ref()?;
                    let width = edit.response.rect.right() - edit.galley_pos.x;
                    live_format::paint_overlays(
                        ui.painter(),
                        backdrop,
                        &cache.overlays,
                        edit.galley_pos,
                        width,
                        style,
                    );
                    task_checkbox_clicked(ui, editor_id, &cache.overlays, edit.galley_pos, style)
                });
                if !edit.response.has_focus()
                    && let Some(range) = edit.state.cursor.char_range()
                {
                    edit_ops::paint_inactive_selection(ui, &edit.galley, edit.galley_pos, range);
                }
                // Not on the frame the text changed: the marks are for the
                // text before.
                if squiggles == Some(seq.get()) {
                    crate::spell::paint_squiggles(
                        ui.painter(),
                        &edit.galley,
                        edit.galley_pos,
                        &spelling.shown_chars,
                        ui.visuals().error_fg_color,
                    );
                }
                // A right-click: the char under the pointer, for the menu.
                let menu_char = if edit.response.secondary_clicked() {
                    edit.response
                        .interact_pointer_pos()
                        .map(|pos| edit.galley.cursor_from_pos(pos - edit.galley_pos).index)
                } else {
                    None
                };
                if scroll_to_cursor && let Some(range) = edit.state.cursor.char_range() {
                    let caret = edit
                        .galley
                        .pos_from_cursor(range.primary)
                        .translate(edit.galley_pos.to_vec2());
                    // Jump like Notepad (no smooth-scroll animation).
                    ui.scroll_to_rect_animation(
                        caret,
                        Some(egui::Align::Center),
                        egui::style::ScrollAnimation::none(),
                    );
                }
                let pressed = edit.response.clicked()
                    || (edit.response.is_pointer_button_down_on()
                        && ui.input(|i| i.pointer.any_pressed()));
                (
                    edit.response,
                    edit.state.cursor.char_range(),
                    pressed,
                    task_toggle,
                    menu_char,
                )
            });
            tab.editor_scroll_id = Some(output.id);
            let (response, mut cursor, pressed, task_toggle, menu_char) = output.inner;
            // Keep the caret out of `\r\n` pairs (checked only when it moved).
            let right_pressed = ui.input(|i| i.key_pressed(egui::Key::ArrowRight));
            let mut snapped = None;
            if let Some(range) = cursor
                && Some(range) != cursor_before
            {
                snapped = edit_ops::cursor_out_of_crlf(
                    buffer.text.as_str(),
                    range,
                    cursor_before,
                    right_pressed,
                );
                // A click in the hidden marks at the start of a line
                // (`## `, `> `, `- `) lands before the text after them.
                if pressed
                    && range.is_empty()
                    && let Some(reveal) = reveal
                {
                    let analysis = document::formatted_analysis(
                        formatted_analysis,
                        seq.get(),
                        buffer.text.as_str(),
                    );
                    let line = analysis.reveal_for(range.primary.index, range.primary.index);
                    if !reveal_covers(reveal, line)
                        && let Some(after) = analysis.after_line_prefix(range.primary.index)
                    {
                        snapped = Some(CCursorRange::one(CCursor::new(after)));
                    }
                }
            }
            if let Some(snapped) = snapped {
                let mut state = egui::TextEdit::load_state(ui.ctx(), editor_id).unwrap_or_default();
                state.cursor.set_char_range(Some(snapped));
                state.store(ui.ctx(), editor_id);
                cursor = Some(snapped);
            }
            // The caret moved to other lines: lay out again with their
            // marks shown before anything is painted.
            let mut discarded = false;
            if let (Some(reveal), Some(range)) = (reveal, cursor) {
                let analysis = document::formatted_analysis(
                    formatted_analysis,
                    seq.get(),
                    buffer.text.as_str(),
                );
                if analysis.reveal_for(range.primary.index, range.secondary.index) != reveal {
                    ui.ctx()
                        .request_discard("formatted view: the caret moved to another line");
                    discarded = true;
                }
            }
            if focus_editor {
                response.request_focus();
            }
            let changed = response.changed();
            (
                response,
                menu_char,
                changed,
                seq.get(),
                task_toggle,
                discarded,
            )
        };

        tab.doc.edit_seq = next_seq;
        if changed {
            tab.note_text_changed(true);
        }
        if discarded && scroll_to_cursor {
            // The scroll request of the discarded pass is lost.
            self.editor_ops.scroll_to_cursor = true;
        }
        if let Some(byte) = task_toggle {
            self.toggle_task(ui.ctx(), byte);
        }
        self.editor_context_menu(ui.ctx(), &response, menu_char);
    }

    /// Tick or untick the task checkbox whose state char (between the
    /// brackets) is at `byte`, as one undoable edit.
    pub(crate) fn toggle_task(&mut self, ctx: &egui::Context, byte: usize) {
        let tab = self.tab_mut();
        let replacement = match tab.doc.text.as_bytes().get(byte) {
            Some(b' ') => "x",
            Some(b'x' | b'X') => " ",
            _ => return,
        };
        let id = tab.editor_id();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let cursor = state.cursor.char_range().unwrap_or_default();
        let mut undoer = state.undoer();
        undoer.add_undo(&(cursor, tab.doc.text.as_str().to_owned()));
        Arc::make_mut(&mut tab.doc.text).replace_range(byte..=byte, replacement);
        undoer.add_undo(&(cursor, tab.doc.text.as_str().to_owned()));
        state.set_undoer(undoer);
        state.store(ctx, id);
        tab.bump_edit_seq();
        tab.note_text_changed(false);
    }

    /// The preview, in `ui` (a document panel, at the base text sizes).
    pub(crate) fn show_preview(&mut self, ui: &mut egui::Ui) {
        ui_style::TextSizes::base().apply(ui.style_mut());
        let mode = self.mode;
        let heading_color_mode = self.heading_color_mode;
        let tab = self.tab_mut();
        if mode == Mode::SideBySide
            && let Some(remaining) = tab.doc.debounce_remaining(DEBOUNCE)
        {
            ui.ctx().request_repaint_after(remaining);
            return;
        }

        tab.doc.consume_preview_dirty();

        tab.ensure_preview_style(ui.visuals(), heading_color_mode);

        // Consume any pending nav-scroll target and pass it directly to the
        // ScrollArea, avoiding the ID-mismatch problem with external state lookup.
        let scroll_y = tab.nav.pending_preview_scroll_y.take();

        if let Some(ref style) = tab.preview_style_cache.style {
            // Salt with the tab id so each tab keeps its own preview scroll offset.
            ui.push_id(tab.id, |ui| {
                MarkdownViewer::new("preview_markdown").show_scrollable(
                    ui,
                    &mut tab.doc.preview_cache,
                    style,
                    tab.doc.text.as_str(),
                    scroll_y,
                );
            });
        }
    }

    /// Notepad-style "Do you want to save changes to <name>?" modal for the
    /// tab at the front of the close prompt.
    pub(crate) fn show_close_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &self.close_prompt else {
            return;
        };
        let remaining = prompt.queue.len().saturating_sub(1);
        let Some(index) = prompt.queue.first().and_then(|id| self.tab_index(*id)) else {
            self.close_prompt = None;
            return;
        };
        let name = self.tabs[index].title().into_owned();

        const CHOICES: [CloseChoice; 3] = [
            CloseChoice::Save,
            CloseChoice::DontSave,
            CloseChoice::Cancel,
        ];
        let answer = crate::dialog::show(
            ctx,
            "close_prompt",
            crate::app_identity::APP_NAME,
            |ui| {
                ui.label(format!("Do you want to save changes to {name}?"));
                if remaining > 0 {
                    ui.weak(format!("{remaining} more unsaved after this one."));
                }
            },
            &[
                ("Save", crate::dialog::Role::Primary),
                ("Don't save", crate::dialog::Role::Secondary),
                ("Cancel", crate::dialog::Role::Secondary),
            ],
            Some(2),
        );
        let choice = answer.and_then(|index| CHOICES.get(index).copied());
        if let Some(choice) = choice {
            self.resolve_close_prompt(choice);
        }
    }

    /// The active tab's file was changed by another program while it has
    /// unsaved edits: ask what to do (a background tab's question waits,
    /// marked `⚠` in the tab strip, until the user switches to it).  Closing
    /// the dialog is Ignore.
    pub(crate) fn show_disk_change_dialog(&mut self, ctx: &egui::Context) {
        use crate::dialog::Role;

        const CHOICES: [DiskChangeChoice; 3] = [
            DiskChangeChoice::SaveAs,
            DiskChangeChoice::Ignore,
            DiskChangeChoice::Reload,
        ];
        let tab = self.tab_mut();
        if tab.disk.pending_change.is_none() {
            return;
        }
        let title = format!("“{}” was changed by another program", tab.title());
        let answer = crate::dialog::show(
            ctx,
            "disk_change",
            &title,
            |ui| {
                ui.label("You have unsaved edits in this file.");
                ui.label(
                    "Save as: save your text as a new file; this one stays as it is.\n\
                     Ignore: keep your text as it is; saving replaces the file.\n\
                     Reload: open the file's new version; your edits are lost.",
                );
            },
            &[
                ("Save as…", Role::Primary),
                ("Ignore", Role::Secondary),
                ("Reload", Role::Destructive),
            ],
            Some(1),
        );
        if let Some(choice) = answer.and_then(|index| CHOICES.get(index).copied()) {
            tab.apply_disk_change_choice(choice);
        }
    }
}

impl Tab {
    /// Rebuild the cached `MarkdownStyle` when the theme, colour mode, or
    /// image URI changes; otherwise reuse the previous value.
    pub(crate) fn ensure_preview_style(&mut self, visuals: &egui::Visuals, colored: bool) {
        let dark = visuals.dark_mode;
        let uri = &self.doc.image_uri_scheme;
        let c = &self.preview_style_cache;

        if c.style.is_some() && c.dark_mode == dark && c.colored == colored && c.image_uri == *uri {
            return;
        }

        let mut style = if colored {
            MarkdownStyle::colored(visuals)
        } else {
            MarkdownStyle::from_visuals(visuals)
        };
        style.image_base_uri.clone_from(uri);
        style.centered_column = true;
        let c = &mut self.preview_style_cache;
        c.dark_mode = dark;
        c.colored = colored;
        c.image_uri.clone_from(uri);
        c.style = Some(style);
    }
}
