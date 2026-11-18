//! Notepad-style status bar:
//! `Ln X, Col Y | N characters … Edit Raw Split Preview | 100% | Windows (CRLF) | UTF-8`.
//!
//! Disk / error notices for the active tab show in the middle; they stay
//! visible (in a slim bar) even when View → Status bar is off.

use eframe::egui;

use crate::{
    AppCommand, Mode, RustdownApp, edit_ops::line_col, line_ending::LineEnding, live_format,
    tab::TabId,
};

/// Per-document and per-caret values cached between frames so big files are
/// not rescanned every frame.
#[derive(Default)]
pub(crate) struct StatusCache {
    doc_key: Option<(TabId, u64)>,
    chars: usize,
    line_ending: Option<LineEnding>,
    caret_key: Option<(TabId, u64, usize)>,
    line_col: (usize, usize),
}

/// Status-bar hint shown left of the mode switch while Edit shows a large
/// document as raw text.
pub(crate) const LARGE_FILE_LABEL: &str = "Raw (large file)";

/// One frame's status-bar values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatusInfo {
    /// 1-based caret line and column; `None` without an editor.
    pub(crate) line_col: Option<(usize, usize)>,
    /// Selected chars (0 without a selection).
    pub(crate) selected: usize,
    pub(crate) chars: usize,
    pub(crate) line_ending: LineEnding,
}

impl RustdownApp {
    /// Compute (or reuse) the status values for the active tab.  The text
    /// scan reruns once per edit, but waits for the stats debounce while
    /// the user is typing.
    pub(crate) fn status_info(&mut self, ctx: &egui::Context) -> StatusInfo {
        let selection = self.mode.uses_editor().then(|| {
            egui::TextEdit::load_state(ctx, self.tab().editor_id())
                .and_then(|state| state.cursor.char_range())
                .map_or((0, 0), |range| (range.primary.index, range.secondary.index))
        });
        let tab = &self.tabs[self.active];
        let text = tab.doc.text.as_str();
        let seq = tab.doc.edit_seq;
        let cache = &mut self.status_cache;

        let other_tab = cache.doc_key.is_none_or(|(id, _)| id != tab.id);
        if cache.doc_key != Some((tab.id, seq)) && (other_tab || !tab.doc.stats_dirty) {
            cache.chars = text.chars().count();
            cache.line_ending = LineEnding::detect(text);
            cache.doc_key = Some((tab.id, seq));
        }

        let line_col = selection.map(|(caret, _)| {
            let key = (tab.id, seq, caret);
            if cache.caret_key != Some(key) {
                cache.line_col = line_col(text, caret);
                cache.caret_key = Some(key);
            }
            cache.line_col
        });
        StatusInfo {
            line_col,
            selected: selection.map_or(0, |(caret, anchor)| caret.abs_diff(anchor)),
            chars: cache.chars,
            // Without line breaks: what Enter inserts in this document.
            line_ending: cache
                .line_ending
                .unwrap_or_else(|| tab.doc.line_ending.into()),
        }
    }

    /// Bottom status bar (see the module docs).
    pub(crate) fn show_status_bar(&mut self, ctx: &egui::Context) {
        let has_notice = self.tab().error.is_some();
        if !self.status_bar_visible && !has_notice {
            return;
        }
        let info = self.status_bar_visible.then(|| self.status_info(ctx));
        let mode = self.mode;
        let large_file_hint = self.large_file_hint();
        let zoom = self.zoom;
        let stats = self.tab().doc.stats;

        let mut cmds = Vec::new();
        let mut clear_error = false;
        let frame = egui::Frame::new()
            .fill(ctx.style().visuals.panel_fill)
            .inner_margin(egui::Margin::symmetric(10, 2));
        egui::TopBottomPanel::bottom("status")
            .frame(frame)
            .show(ctx, |ui| {
                ui.style_mut().override_text_style = Some(egui::TextStyle::Small);
                ui.horizontal(|ui| {
                    if let Some(info) = info {
                        if let Some((line, col)) = info.line_col {
                            let position = if info.selected > 0 {
                                format!("Ln {line}, Col {col}, {} selected", info.selected)
                            } else {
                                format!("Ln {line}, Col {col}")
                            };
                            ui.label(position);
                            ui.separator();
                        }
                        ui.label(format!("{} characters", info.chars))
                            .on_hover_text(format!(
                                "{} words · {} lines",
                                stats.words, stats.lines
                            ));
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(info) = info {
                            ui.label("UTF-8");
                            ui.separator();
                            ui.label(info.line_ending.label());
                            ui.separator();
                            ui.label(format!("{:.0}%", zoom * 100.0))
                                .on_hover_text("Zoom (Ctrl+Plus / Ctrl+Minus, Ctrl+0 to reset)");
                            ui.separator();
                            // Right to left: shows as Edit · Raw · Split · Preview.
                            for candidate in Mode::ALL.into_iter().rev() {
                                if ui
                                    .selectable_label(mode == candidate, candidate.short_label())
                                    .on_hover_text(candidate.tooltip())
                                    .clicked()
                                {
                                    cmds.push(AppCommand::SetMode(candidate));
                                }
                            }
                            ui.separator();
                            if let Some(hover) = &large_file_hint {
                                ui.weak(LARGE_FILE_LABEL).on_hover_text(hover);
                                ui.separator();
                            }
                        }
                        self.status_notices(ui, &mut clear_error);
                    });
                });
            });

        if clear_error {
            self.tab_mut().error = None;
        }
        for cmd in cmds {
            self.execute(cmd, ctx);
        }
    }

    /// The hover text of the [`LARGE_FILE_LABEL`] hint, while Edit shows
    /// the active document as raw text because it is too large for the
    /// formatted view; `None` otherwise.
    pub(crate) fn large_file_hint(&self) -> Option<String> {
        (self.mode == Mode::Edit && self.formatted_view_too_large()).then(|| {
            format!(
                "Files over {} show as raw text in Edit: the formatted view would slow typing down",
                live_format::MAX_FORMATTED_LABEL
            )
        })
    }

    /// The active tab's error notice (right-to-left layout).
    fn status_notices(&self, ui: &mut egui::Ui, clear_error: &mut bool) {
        if let Some(error) = self.tab().error.as_deref() {
            if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                *clear_error = true;
            }
            ui.add(
                egui::Label::new(egui::RichText::new(error).color(ui.visuals().error_fg_color))
                    .truncate(),
            )
            .on_hover_text(error);
        }
    }
}

#[cfg(test)]
mod app_tests {
    use std::sync::Arc;

    use eframe::egui::text::{CCursor, CCursorRange};

    use super::*;

    fn set_selection(ctx: &egui::Context, app: &RustdownApp, caret: usize, anchor: usize) {
        let id = app.tab().editor_id();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange {
            primary: CCursor::new(caret),
            secondary: CCursor::new(anchor),
            h_pos: None,
        }));
        state.store(ctx, id);
    }

    #[test]
    fn status_info_reports_caret_selection_chars_and_line_ending() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp::default();
        app.tab_mut().doc.text = Arc::new("héllo\r\nwörld\r\n".to_owned());

        let info = app.status_info(&ctx);
        assert_eq!(info.line_col, Some((1, 1)), "before the editor was shown");
        assert_eq!(info.chars, 14);
        assert_eq!(info.line_ending, LineEnding::Crlf);
        assert_eq!(info.selected, 0);

        set_selection(&ctx, &app, 10, 7);
        let info = app.status_info(&ctx);
        assert_eq!(info.line_col, Some((2, 4)));
        assert_eq!(info.selected, 3);

        // Preview: no caret.
        app.mode = Mode::Preview;
        let info = app.status_info(&ctx);
        assert_eq!(info.line_col, None);
        assert_eq!(info.selected, 0);
    }

    #[test]
    fn status_scan_is_cached_per_edit_and_debounced_while_typing() {
        let ctx = egui::Context::default();
        let mut app = RustdownApp::default();
        app.tab_mut().doc.text = Arc::new("a\nb".to_owned());
        assert_eq!(app.status_info(&ctx).line_ending, LineEnding::Lf);

        // Typing: text changed but the stats debounce has not fired yet.
        let tab = app.tab_mut();
        tab.doc.text = Arc::new("a\r\nb\r\n".to_owned());
        tab.bump_edit_seq();
        tab.note_text_changed(true);
        let info = app.status_info(&ctx);
        assert_eq!(info.line_ending, LineEnding::Lf, "stale until the debounce");
        assert_eq!(info.chars, 3);

        app.tab_mut().refresh_stats_now();
        let info = app.status_info(&ctx);
        assert_eq!(info.line_ending, LineEnding::Crlf);
        assert_eq!(info.chars, 6);

        // Another tab is scanned straight away.
        let mut other = crate::tab::Tab::default();
        other.doc.text = Arc::new("x\ry".to_owned());
        other.doc.stats_dirty = true;
        app.tabs.push(other);
        app.activate_tab(1);
        assert_eq!(app.status_info(&ctx).line_ending, LineEnding::Cr);

        // No line breaks: what Enter inserts (a new document's style, which
        // is CRLF on Windows).
        app.tab_mut().doc.text = Arc::new("x".to_owned());
        app.tab_mut().bump_edit_seq();
        app.tab_mut().refresh_stats_now();
        let new_doc = if cfg!(windows) {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        };
        assert_eq!(app.status_info(&ctx).line_ending, new_doc);
    }

    #[test]
    fn status_bar_renders_and_hides_but_keeps_notices() {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        let mut app = RustdownApp::default();
        let frame = |app: &mut RustdownApp| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| app.show_status_bar(ctx));
            ctx.memory(|m| m.area_rect(egui::Id::new("status")).is_some())
                || egui::containers::panel::PanelState::load(&ctx, egui::Id::new("status"))
                    .is_some()
        };
        assert!(frame(&mut app));
        app.execute(AppCommand::ToggleStatusBar, &ctx);
        assert!(!app.status_bar_visible);
        let ctx2 = egui::Context::default();
        let _ = ctx2.run(egui::RawInput::default(), |ctx| app.show_status_bar(ctx));
        assert!(
            egui::containers::panel::PanelState::load(&ctx2, egui::Id::new("status")).is_none(),
            "hidden"
        );
        app.tab_mut().error = Some("Save failed: disk full".to_owned());
        let _ = ctx2.run(egui::RawInput::default(), |ctx| app.show_status_bar(ctx));
        assert!(
            egui::containers::panel::PanelState::load(&ctx2, egui::Id::new("status")).is_some(),
            "notices stay visible"
        );
    }
}
