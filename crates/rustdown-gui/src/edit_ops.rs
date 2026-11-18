//! Edit-menu commands that act on the active editor (undo / redo,
//! clipboard, delete, select all, time/date, find next / previous, go to
//! line) plus the text-position helpers they and the status bar share.
//!
//! egui's `TextEdit` already handles Ctrl+Z / Ctrl+Y / Ctrl+X / Ctrl+C /
//! Ctrl+V / Del / Ctrl+A itself while it has keyboard focus, so those keys
//! are not bound here.  The menu items reuse that code path: they queue the
//! equivalent input events and inject them into the editor's input on the
//! next frame in which it has focus (a menu click always takes focus away
//! from the editor first).  Paste asks the platform integration for the
//! clipboard (`ViewportCommand::RequestPaste`), which delivers an
//! `Event::Paste` to the then-focused editor on the next frame.

use std::ops::Range;

use eframe::egui::{
    self, Key, Modifiers,
    text::{CCursor, CCursorRange},
};

use crate::{AppCommand, RustdownApp, tab::TabId};

/// egui id of the find bar's query field.
pub(crate) const FIND_QUERY_ID: &str = "search-query";

/// Editor work queued by menu commands; see the module docs.
#[derive(Default)]
pub(crate) struct EditorOps {
    /// Input events for the active editor, injected once it has focus.
    pending: Vec<egui::Event>,
    /// Tab the pending events were queued for (dropped if it changes).
    pending_tab: Option<TabId>,
    /// Scroll the caret / selection into view on the next editor render
    /// (after Go to line or Find next moved it).
    pub(crate) scroll_to_cursor: bool,
    /// Undo / redo availability while the Edit menu is open.
    undo_cache: Option<UndoCache>,
}

#[derive(Clone, Copy)]
struct UndoCache {
    tab: TabId,
    edit_seq: u64,
    undo: bool,
    redo: bool,
}

/// Which Edit-menu items can apply right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct EditAvailability {
    /// The editor is shown (Edit or Side-by-side mode).
    pub(crate) editor: bool,
    pub(crate) selection: bool,
    pub(crate) text: bool,
    pub(crate) undo: bool,
    pub(crate) redo: bool,
}

impl EditAvailability {
    /// `false` when `cmd` would do nothing (the menu greys it out).
    pub(crate) const fn allows(self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::Undo => self.editor && self.undo,
            AppCommand::Redo => self.editor && self.redo,
            AppCommand::Cut | AppCommand::Copy | AppCommand::Delete => {
                self.editor && self.selection
            }
            AppCommand::Paste
            | AppCommand::PasteImage
            | AppCommand::InsertTimeDate
            | AppCommand::GoToLine => self.editor,
            AppCommand::SelectAll | AppCommand::FindNext | AppCommand::FindPrevious => {
                self.editor && self.text
            }
            _ => true,
        }
    }
}

const fn key_event(key: Key, modifiers: Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// The editor input event equivalent to an Edit-menu command, if any.
pub(crate) fn editor_event_for(cmd: &AppCommand) -> Option<egui::Event> {
    Some(match cmd {
        AppCommand::Undo => key_event(Key::Z, Modifiers::COMMAND),
        AppCommand::Redo => key_event(Key::Y, Modifiers::COMMAND),
        AppCommand::Cut => egui::Event::Cut,
        AppCommand::Copy => egui::Event::Copy,
        AppCommand::Delete => key_event(Key::Delete, Modifiers::NONE),
        AppCommand::SelectAll => key_event(Key::A, Modifiers::COMMAND),
        AppCommand::InsertTimeDate => egui::Event::Text(notepad_time_date(now_local())),
        _ => return None,
    })
}

impl RustdownApp {
    /// Replace bytes `range` of the active tab's text with `with`, as one
    /// undoable edit, and select bytes `select` of the new text.
    pub(crate) fn replace_text_undoable(
        &mut self,
        ctx: &egui::Context,
        range: Range<usize>,
        with: &str,
        select: Range<usize>,
    ) {
        let tab = self.tab_mut();
        if tab.doc.text.get(range.clone()).is_none() {
            return;
        }
        let id = tab.editor_id();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let cursor = state.cursor.char_range().unwrap_or_default();
        let mut undoer = state.undoer();
        undoer.add_undo(&(cursor, tab.doc.text.as_str().to_owned()));
        std::sync::Arc::make_mut(&mut tab.doc.text).replace_range(range, with);
        let text = tab.doc.text.as_str();
        let selection = CCursorRange::two(
            CCursor::new(byte_to_char(text, select.start)),
            CCursor::new(byte_to_char(text, select.end)),
        );
        undoer.add_undo(&(selection, text.to_owned()));
        state.cursor.set_char_range(Some(selection));
        state.set_undoer(undoer);
        state.store(ctx, id);
        tab.bump_edit_seq();
        tab.note_text_changed(false);
    }

    /// Queue `event` for the active editor (no-op without an editor).
    pub(crate) fn queue_editor_event(&mut self, ctx: &egui::Context, event: egui::Event) {
        if !self.mode.uses_editor() {
            return;
        }
        let tab = self.tab().id;
        if self.editor_ops.pending_tab != Some(tab) {
            self.editor_ops.pending.clear();
        }
        self.editor_ops.pending_tab = Some(tab);
        self.editor_ops.pending.push(event);
        self.focus_editor = true;
        ctx.request_repaint();
    }

    /// Edit → Paste: the integration reads the clipboard and sends an
    /// `Event::Paste` next frame, when the editor will have focus again.
    pub(crate) fn paste_into_editor(&mut self, ctx: &egui::Context) {
        if !self.mode.uses_editor() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        self.focus_editor = true;
        ctx.request_repaint();
    }

    /// Called by `show_editor` before the editor is built.  Injects the
    /// queued events when the editor has focus and no click is landing this
    /// frame (a click elsewhere makes egui drop the editor's focus before it
    /// reads input).  Returns `true` when the events must wait: the caller
    /// then gives the editor focus after rendering it.
    pub(crate) fn inject_pending_editor_events(
        &mut self,
        ctx: &egui::Context,
        editor_id: egui::Id,
        tab: TabId,
    ) -> bool {
        let ops = &mut self.editor_ops;
        if ops.pending.is_empty() {
            return false;
        }
        if ops.pending_tab != Some(tab) {
            ops.pending.clear();
            return false;
        }
        let ready = ctx.memory(|m| m.has_focus(editor_id)) && !ctx.input(|i| i.pointer.any_click());
        if ready {
            let events = std::mem::take(&mut ops.pending);
            ctx.input_mut(|input| input.events.extend(events));
            false
        } else {
            ctx.request_repaint();
            true
        }
    }

    /// What the Edit menu may offer right now.
    pub(crate) fn edit_availability(&mut self, ctx: &egui::Context) -> EditAvailability {
        if !self.mode.uses_editor() {
            return EditAvailability::default();
        }
        let tab = self.tab();
        let selection = egui::TextEdit::load_state(ctx, tab.editor_id())
            .and_then(|state| state.cursor.char_range())
            .is_some_and(|range| !range.is_empty());
        let text = !tab.doc.text.is_empty();
        let (undo, redo) = self.undo_redo_available(ctx);
        EditAvailability {
            editor: true,
            selection,
            text,
            undo,
            redo,
        }
    }

    /// Whether the editor's undo history has anything to undo / redo.
    /// Copies the history (each entry holds the full text), so the result
    /// is cached while the Edit menu stays open; see [`Self::forget_undo_cache`].
    fn undo_redo_available(&mut self, ctx: &egui::Context) -> (bool, bool) {
        let tab = &self.tabs[self.active];
        if let Some(cache) = self.editor_ops.undo_cache
            && cache.tab == tab.id
            && cache.edit_seq == tab.doc.edit_seq
        {
            return (cache.undo, cache.redo);
        }
        let (undo, redo) =
            egui::TextEdit::load_state(ctx, tab.editor_id()).map_or((false, false), |state| {
                let cursor = state.cursor.char_range().unwrap_or_default();
                let current = (cursor, tab.doc.text.as_str().to_owned());
                let undoer = state.undoer();
                (undoer.has_undo(&current), undoer.has_redo(&current))
            });
        self.editor_ops.undo_cache = Some(UndoCache {
            tab: tab.id,
            edit_seq: tab.doc.edit_seq,
            undo,
            redo,
        });
        (undo, redo)
    }

    /// Drop the cached undo availability (the Edit menu closed).
    pub(crate) const fn forget_undo_cache(&mut self) {
        self.editor_ops.undo_cache = None;
    }

    /// The active editor's selection as a sorted char range (empty at the
    /// caret; `0..0` before the editor was first shown).
    pub(crate) fn editor_selection(&self, ctx: &egui::Context) -> Range<usize> {
        egui::TextEdit::load_state(ctx, self.tab().editor_id())
            .and_then(|state| state.cursor.char_range())
            .map_or(0..0, |range| {
                let (a, b) = (range.primary.index, range.secondary.index);
                a.min(b)..a.max(b)
            })
    }

    /// Select `chars` (char indices) in the active editor, give it focus
    /// and scroll the selection into view.
    pub(crate) fn select_in_editor(&mut self, ctx: &egui::Context, chars: Range<usize>) {
        let id = self.tab().editor_id();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(chars.start),
            CCursor::new(chars.end),
        )));
        state.store(ctx, id);
        self.focus_editor = true;
        self.editor_ops.scroll_to_cursor = true;
    }

    /// Edit → Find next / Find previous (F3 / Shift+F3): select the next
    /// match of the find-bar query after (or before) the selection,
    /// wrapping around.  With no query, opens the find bar.  Pressed in the
    /// find bar, focus stays there (like Notepad's Find dialog) and the
    /// match shows as an inactive selection.
    pub(crate) fn find_in_editor(&mut self, ctx: &egui::Context, forward: bool) {
        if !self.mode.uses_editor() {
            return;
        }
        if self.search.query.is_empty() {
            self.open_search(false);
            return;
        }
        let selection = self.editor_selection(ctx);
        let text = self.tab().doc.text.as_str();
        let from = char_to_byte(
            text,
            if forward {
                selection.end
            } else {
                selection.start
            },
        );
        let Some(found) = find_match(text, &self.search.query, from, forward) else {
            return;
        };
        let start = byte_to_char(text, found.start);
        let end = start + text[found].chars().count();
        let from_find_bar = ctx.memory(|m| m.has_focus(egui::Id::new(FIND_QUERY_ID)));
        self.select_in_editor(ctx, start..end);
        if from_find_bar {
            self.focus_editor = false;
        }
    }

    /// Move the caret to the start of 1-based `line` (clamped) and scroll
    /// it into view.
    pub(crate) fn go_to_line(&mut self, ctx: &egui::Context, line: usize) {
        if !self.mode.uses_editor() {
            return;
        }
        let caret = line_start_char(self.tab().doc.text.as_str(), line);
        self.select_in_editor(ctx, caret..caret);
    }

    /// File → Save all: save every dirty tab (Save As for Untitled ones).
    /// Stops at the first cancelled dialog or failed save.
    pub(crate) fn save_all(&mut self) {
        for index in 0..self.tabs.len() {
            if !self.tabs[index].doc.dirty {
                continue;
            }
            if self.tabs[index].doc.path.is_none() {
                // Show which document the Save As dialog is for.
                self.activate_tab(index);
            }
            if !self.tabs[index].save_doc(false) {
                return;
            }
        }
    }
}

/// Paint `range` of an unfocused editor as an inactive selection.  egui
/// only paints the focused `TextEdit`'s selection; this keeps a find match
/// (or a selection about to be copied from the menu) visible.
pub(crate) fn paint_inactive_selection(
    ui: &egui::Ui,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    range: CCursorRange,
) {
    if range.is_empty() {
        return;
    }
    let [start, end] = range.sorted_cursors();
    let (a, b) = (galley.pos_from_cursor(start), galley.pos_from_cursor(end));
    let (left, right) = (galley.rect.left(), galley.rect.right().max(a.max.x));
    let mut rects = Vec::with_capacity(3);
    if (a.min.y - b.min.y).abs() < 0.5 {
        rects.push(egui::Rect::from_min_max(a.min, b.max));
    } else {
        rects.push(egui::Rect::from_min_max(a.min, egui::pos2(right, a.max.y)));
        if b.min.y > a.max.y {
            rects.push(egui::Rect::from_min_max(
                egui::pos2(left, a.max.y),
                egui::pos2(right, b.min.y),
            ));
        }
        rects.push(egui::Rect::from_min_max(egui::pos2(left, b.min.y), b.max));
    }
    let color = ui.visuals().selection.bg_fill.gamma_multiply(0.35);
    for rect in rects {
        ui.painter()
            .rect_filled(rect.translate(galley_pos.to_vec2()), 0.0, color);
    }
}

// ── Text positions ─────────────────────────────────────────────────────

/// Byte offset of char index `char_index` (clamped to the end).
#[must_use]
pub(crate) fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte, _)| byte)
}

/// Char index of byte offset `byte` (clamped; must be a char boundary).
#[must_use]
pub(crate) fn byte_to_char(text: &str, byte: usize) -> usize {
    text.get(..byte.min(text.len()))
        .map_or_else(|| text.chars().count(), |prefix| prefix.chars().count())
}

/// `range` with each end moved off the spot between a `\r` and its `\n`,
/// or `None` when neither end is there.  The `\r` is invisible, so that
/// spot looks like the end of the line: egui puts the caret there for End,
/// Up / Down or a click past the end of a line, and the arrow keys stop
/// there (crossing a CRLF took two presses).  Such an end moves back before
/// the `\r`, unless a Right arrow press (`right_pressed`) moved it forward
/// from `before`: then it moves on past the `\n`.
#[must_use]
pub(crate) fn cursor_out_of_crlf(
    text: &str,
    range: CCursorRange,
    before: Option<CCursorRange>,
    right_pressed: bool,
) -> Option<CCursorRange> {
    let snap = |cursor: CCursor, before: Option<CCursor>| {
        if !crate::line_ending::inside_crlf(text, char_to_byte(text, cursor.index)) {
            return None;
        }
        let forward = right_pressed && before.is_some_and(|before| before.index < cursor.index);
        let index = if forward {
            cursor.index + 1
        } else {
            cursor.index - 1
        };
        Some(CCursor { index, ..cursor })
    };
    let primary = snap(range.primary, before.map(|before| before.primary));
    let secondary = if range.secondary.index == range.primary.index {
        primary
    } else {
        snap(range.secondary, before.map(|before| before.secondary))
    };
    (primary.is_some() || secondary.is_some()).then(|| CCursorRange {
        primary: primary.unwrap_or(range.primary),
        secondary: secondary.unwrap_or(range.secondary),
        h_pos: range.h_pos,
    })
}

/// Number of lines as the editor shows them (`\n` separated; a trailing
/// newline starts an empty last line).
#[must_use]
pub(crate) fn line_count(text: &str) -> usize {
    1 + memchr::memchr_iter(b'\n', text.as_bytes()).count()
}

/// 1-based `(line, column)` of char index `char_index`, Notepad style:
/// the column counts chars (not bytes) from the start of the line.
#[must_use]
pub(crate) fn line_col(text: &str, char_index: usize) -> (usize, usize) {
    let byte = char_to_byte(text, char_index);
    let before = &text.as_bytes()[..byte];
    let line = 1 + memchr::memchr_iter(b'\n', before).count();
    let line_start = memchr::memrchr(b'\n', before).map_or(0, |pos| pos + 1);
    let col = 1 + text[line_start..byte].chars().count();
    (line, col)
}

/// Char index of the start of 1-based `line`, clamped to the first / last
/// line.
#[must_use]
pub(crate) fn line_start_char(text: &str, line: usize) -> usize {
    let byte = match line {
        0 | 1 => 0,
        line => memchr::memchr_iter(b'\n', text.as_bytes())
            .take(line - 1)
            .last()
            .map_or(0, |pos| pos + 1),
    };
    byte_to_char(text, byte)
}

/// Byte range of the next match of `query` starting at or after `from`
/// (`forward`), or ending at or before `from` (backward), wrapping around
/// the document.  Case-sensitive, like the find bar's match count.
#[must_use]
pub(crate) fn find_match(
    text: &str,
    query: &str,
    from: usize,
    forward: bool,
) -> Option<Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let (hay, needle) = (text.as_bytes(), query.as_bytes());
    let from = from.min(hay.len());
    let start = if forward {
        memchr::memmem::find(&hay[from..], needle)
            .map(|pos| from + pos)
            .or_else(|| memchr::memmem::find(hay, needle))
    } else {
        memchr::memmem::rfind(&hay[..from], needle).or_else(|| memchr::memmem::rfind(hay, needle))
    }?;
    Some(start..start + needle.len())
}

// ── Time / date (F5) ───────────────────────────────────────────────────

/// A wall-clock time to the minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DateTime {
    pub(crate) year: u32,
    pub(crate) month: u32,
    pub(crate) day: u32,
    pub(crate) hour: u32,
    pub(crate) minute: u32,
}

/// Notepad's F5 format: `h:mm AM/PM M/D/YYYY`.
#[must_use]
pub(crate) fn notepad_time_date(t: DateTime) -> String {
    let (hour, suffix) = match t.hour {
        0 => (12, "AM"),
        hour @ 1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        hour => (hour - 12, "PM"),
    };
    format!(
        "{hour}:{:02} {suffix} {}/{}/{}",
        t.minute, t.month, t.day, t.year
    )
}

/// UTC civil time for `secs` seconds after the Unix epoch (the non-Windows
/// fallback of [`now_local`]).
#[cfg(any(not(windows), test))]
#[must_use]
pub(crate) fn utc_from_unix(secs: u64) -> DateTime {
    // Howard Hinnant's `civil_from_days`, for non-negative day counts.
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    DateTime {
        year: year as u32,
        month: month as u32,
        day: day as u32,
        hour: (rem / 3_600) as u32,
        minute: (rem % 3_600 / 60) as u32,
    }
}

/// The current local time (Windows `GetLocalTime`).
#[cfg(windows)]
#[must_use]
pub(crate) fn now_local() -> DateTime {
    /// Win32 `SYSTEMTIME`.
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        _day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        _second: u16,
        _milliseconds: u16,
    }
    let mut now = SystemTime::default();
    // SAFETY: `GetLocalTime` (kernel32) only writes the `SYSTEMTIME` it is
    // given, which the `&mut` guarantees is valid, aligned and exclusive.
    #[allow(unsafe_code)]
    {
        unsafe extern "system" {
            safe fn GetLocalTime(system_time: &mut SystemTime);
        }
        GetLocalTime(&mut now);
    }
    DateTime {
        year: u32::from(now.year),
        month: u32::from(now.month),
        day: u32::from(now.day),
        hour: u32::from(now.hour),
        minute: u32::from(now.minute),
    }
}

/// The current time; UTC on the (non-Windows) dev host.
#[cfg(not(windows))]
#[must_use]
pub(crate) fn now_local() -> DateTime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    utc_from_unix(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_chars_from_one() {
        let text = "ab\ncdé\n\nxyz";
        let cases = [
            (0, (1, 1)),
            (2, (1, 3)), // end of line 1, before '\n'
            (3, (2, 1)),
            (6, (2, 4)), // after the 2-byte 'é': chars, not bytes
            (7, (3, 1)),
            (8, (4, 1)),
            (11, (4, 4)),
            (99, (4, 4)), // clamped to the end
        ];
        for (index, expected) in cases {
            assert_eq!(line_col(text, index), expected, "char {index}");
        }
        assert_eq!(line_col("", 0), (1, 1));
    }

    #[test]
    fn line_col_with_crlf_and_multibyte() {
        // "日本\r\n🦀x\r\n": the '\r' belongs to the line it ends.
        let text = "日本\r\n🦀x\r\n";
        assert_eq!(line_col(text, 2), (1, 3)); // before '\r'
        assert_eq!(line_col(text, 4), (2, 1)); // after "\r\n"
        assert_eq!(line_col(text, 5), (2, 2)); // after the 4-byte crab
        assert_eq!(line_col(text, 6), (2, 3));
        assert_eq!(line_col(text, 8), (3, 1));
        assert_eq!(line_count(text), 3);
    }

    #[test]
    fn char_and_byte_conversions() {
        let text = "aé🦀b";
        assert_eq!(char_to_byte(text, 0), 0);
        assert_eq!(char_to_byte(text, 2), 3);
        assert_eq!(char_to_byte(text, 3), 7);
        assert_eq!(char_to_byte(text, 10), text.len());
        assert_eq!(byte_to_char(text, 7), 3);
        assert_eq!(byte_to_char(text, 100), 4);
    }

    #[test]
    fn go_to_line_start_is_clamped() {
        let text = "one\ntwo\r\nthrée\nfour";
        assert_eq!(line_count(text), 4);
        assert_eq!(line_start_char(text, 0), 0, "0 clamps to the first line");
        assert_eq!(line_start_char(text, 1), 0);
        assert_eq!(line_start_char(text, 2), 4);
        assert_eq!(line_start_char(text, 3), 9);
        assert_eq!(line_start_char(text, 4), 15, "after the 2-byte 'é'");
        assert_eq!(
            line_start_char(text, 5),
            15,
            "past the end clamps to the last line"
        );
        assert_eq!(line_start_char(text, usize::MAX), 15);
        assert_eq!(line_start_char("", 3), 0);
        assert_eq!(line_start_char("a\n", 2), 2, "empty last line");
    }

    #[test]
    fn find_match_wraps_both_ways() {
        let text = "foo bar foo baz";
        assert_eq!(find_match(text, "foo", 0, true), Some(0..3));
        assert_eq!(find_match(text, "foo", 3, true), Some(8..11));
        assert_eq!(
            find_match(text, "foo", 11, true),
            Some(0..3),
            "wraps to start"
        );
        assert_eq!(find_match(text, "foo", 8, false), Some(0..3));
        assert_eq!(
            find_match(text, "foo", 0, false),
            Some(8..11),
            "wraps to end"
        );
        assert_eq!(find_match(text, "foo", 100, true), Some(0..3));
        assert_eq!(find_match(text, "nope", 0, true), None);
        assert_eq!(find_match(text, "", 0, true), None);
        assert_eq!(find_match("ça ça", "ça", 2, true), Some(4..7));
    }

    #[test]
    fn notepad_time_date_format() {
        let at = |hour, minute| DateTime {
            year: 2026,
            month: 3,
            day: 7,
            hour,
            minute,
        };
        assert_eq!(notepad_time_date(at(0, 5)), "12:05 AM 3/7/2026");
        assert_eq!(notepad_time_date(at(9, 41)), "9:41 AM 3/7/2026");
        assert_eq!(notepad_time_date(at(12, 0)), "12:00 PM 3/7/2026");
        assert_eq!(notepad_time_date(at(23, 59)), "11:59 PM 3/7/2026");
    }

    #[test]
    fn utc_from_unix_known_dates() {
        let dt = |year, month, day, hour, minute| DateTime {
            year,
            month,
            day,
            hour,
            minute,
        };
        assert_eq!(utc_from_unix(0), dt(1970, 1, 1, 0, 0));
        assert_eq!(utc_from_unix(951_782_400), dt(2000, 2, 29, 0, 0));
        assert_eq!(utc_from_unix(1_700_000_000), dt(2023, 11, 14, 22, 13));
        assert_eq!(utc_from_unix(4_102_444_799), dt(2099, 12, 31, 23, 59));
        let now = now_local();
        assert!((1..=12).contains(&now.month) && (1..=31).contains(&now.day));
    }

    #[test]
    fn edit_availability_rules() {
        let none = EditAvailability::default();
        let cmds = [
            AppCommand::Undo,
            AppCommand::Redo,
            AppCommand::Cut,
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::Delete,
            AppCommand::SelectAll,
            AppCommand::InsertTimeDate,
            AppCommand::GoToLine,
            AppCommand::FindNext,
            AppCommand::FindPrevious,
        ];
        for cmd in &cmds {
            assert!(!none.allows(cmd), "{cmd:?} needs the editor");
        }
        assert!(none.allows(&AppCommand::Find), "find bar works in Preview");
        assert!(none.allows(&AppCommand::FormatDocument));

        let editor = EditAvailability {
            editor: true,
            ..EditAvailability::default()
        };
        for cmd in [
            AppCommand::Paste,
            AppCommand::InsertTimeDate,
            AppCommand::GoToLine,
        ] {
            assert!(editor.allows(&cmd), "{cmd:?}");
        }
        for cmd in [
            AppCommand::Cut,
            AppCommand::Copy,
            AppCommand::Delete,
            AppCommand::Undo,
            AppCommand::Redo,
            AppCommand::SelectAll,
        ] {
            assert!(
                !editor.allows(&cmd),
                "{cmd:?} greyed without selection/text/history"
            );
        }

        let all = EditAvailability {
            editor: true,
            selection: true,
            text: true,
            undo: true,
            redo: true,
        };
        for cmd in &cmds {
            assert!(all.allows(cmd), "{cmd:?}");
        }
    }

    #[test]
    fn editor_events_for_menu_commands() {
        assert_eq!(editor_event_for(&AppCommand::Cut), Some(egui::Event::Cut));
        assert_eq!(editor_event_for(&AppCommand::Copy), Some(egui::Event::Copy));
        assert!(matches!(
            editor_event_for(&AppCommand::Undo),
            Some(egui::Event::Key { key: Key::Z, modifiers, .. }) if modifiers.command
        ));
        assert!(matches!(
            editor_event_for(&AppCommand::InsertTimeDate),
            Some(egui::Event::Text(text)) if text.contains(['A', 'P'])
        ));
        assert_eq!(
            editor_event_for(&AppCommand::Paste),
            None,
            "uses RequestPaste"
        );
        assert_eq!(editor_event_for(&AppCommand::Save), None);
    }
}

/// Headless tests of the menu commands against a real `TextEdit`.
#[cfg(test)]
mod editor_tests {
    use std::sync::Arc;

    use super::*;
    use crate::Mode;

    fn ctx_with_fonts() -> egui::Context {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx
    }

    fn app_with_text(text: &str) -> RustdownApp {
        let mut app = RustdownApp::default();
        app.tab_mut().doc.text = Arc::new(text.to_owned());
        app
    }

    fn editor_frame_with(
        ctx: &egui::Context,
        app: &mut RustdownApp,
        input: egui::RawInput,
    ) -> egui::FullOutput {
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
        })
    }

    fn editor_frame(ctx: &egui::Context, app: &mut RustdownApp) -> egui::FullOutput {
        editor_frame_with(ctx, app, crate::diagnostics::diagnostics_raw_input())
    }

    fn copied(output: &egui::FullOutput) -> Vec<String> {
        output
            .platform_output
            .commands
            .iter()
            .filter_map(|cmd| match cmd {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn text(app: &RustdownApp) -> &str {
        app.tab().doc.text.as_str()
    }

    /// Render, then select `chars` and let the editor take focus.
    fn focused_with_selection(ctx: &egui::Context, app: &mut RustdownApp, chars: Range<usize>) {
        let _ = editor_frame(ctx, app);
        app.select_in_editor(ctx, chars);
        let _ = editor_frame(ctx, app);
        assert!(ctx.memory(|m| m.has_focus(app.tab().editor_id())));
    }

    #[test]
    fn menu_copy_cut_undo_redo_act_on_the_editor() {
        let ctx = ctx_with_fonts();
        let mut app = app_with_text("hello world");
        focused_with_selection(&ctx, &mut app, 0..5);

        app.execute(AppCommand::Copy, &ctx);
        let out = editor_frame(&ctx, &mut app);
        assert_eq!(copied(&out), ["hello"]);
        assert_eq!(text(&app), "hello world", "copy leaves the text");

        let seq = app.tab().doc.edit_seq;
        app.execute(AppCommand::Cut, &ctx);
        let out = editor_frame(&ctx, &mut app);
        assert_eq!(copied(&out), ["hello"]);
        assert_eq!(text(&app), " world");
        assert!(app.tab().doc.dirty, "cut marks the tab dirty");
        assert_ne!(app.tab().doc.edit_seq, seq, "cut bumps edit_seq");

        assert!(app.edit_availability(&ctx).undo);
        app.execute(AppCommand::Undo, &ctx);
        let _ = editor_frame(&ctx, &mut app);
        assert_eq!(text(&app), "hello world");

        app.forget_undo_cache();
        assert!(app.edit_availability(&ctx).redo);
        app.execute(AppCommand::Redo, &ctx);
        let _ = editor_frame(&ctx, &mut app);
        assert_eq!(text(&app), " world");
    }

    #[test]
    fn menu_select_all_delete_and_time_date() {
        let ctx = ctx_with_fonts();
        let mut app = app_with_text("abc\ndef");
        focused_with_selection(&ctx, &mut app, 1..1);

        app.execute(AppCommand::SelectAll, &ctx);
        let _ = editor_frame(&ctx, &mut app);
        assert_eq!(app.editor_selection(&ctx), 0..7);

        app.execute(AppCommand::Delete, &ctx);
        let _ = editor_frame(&ctx, &mut app);
        assert_eq!(text(&app), "");

        app.execute(AppCommand::InsertTimeDate, &ctx);
        let _ = editor_frame(&ctx, &mut app);
        let inserted = text(&app);
        assert!(
            inserted.contains(" AM ") || inserted.contains(" PM "),
            "{inserted:?}"
        );
        assert_eq!(inserted.matches('/').count(), 2, "{inserted:?}");
    }

    #[test]
    fn menu_command_waits_for_focus_after_a_menu_click() {
        let ctx = ctx_with_fonts();
        let mut app = app_with_text("hello");
        let _ = editor_frame(&ctx, &mut app); // never focused (like after a menu click)
        app.select_in_editor(&ctx, 0..5);
        app.focus_editor = false;
        app.execute(AppCommand::Cut, &ctx);
        let _ = editor_frame(&ctx, &mut app);
        assert_eq!(text(&app), "hello", "not focused yet: the cut waits");
        assert!(ctx.memory(|m| m.has_focus(app.tab().editor_id())));
        let _ = editor_frame(&ctx, &mut app);
        assert_eq!(text(&app), "", "applied once the editor has focus");
    }

    #[test]
    fn menu_paste_requests_clipboard_and_pastes_next_frame() {
        let ctx = ctx_with_fonts();
        let mut app = app_with_text("ab");
        focused_with_selection(&ctx, &mut app, 1..1);
        let out = ctx.run(crate::diagnostics::diagnostics_raw_input(), |ctx| {
            app.execute(AppCommand::Paste, ctx);
            egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
        });
        let requested = out.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .contains(&egui::ViewportCommand::RequestPaste)
        });
        assert!(requested, "asks the integration for the clipboard");
        // The integration answers with an `Event::Paste` next frame.
        let mut input = crate::diagnostics::diagnostics_raw_input();
        input.events.push(egui::Event::Paste("XY".to_owned()));
        let _ = editor_frame_with(&ctx, &mut app, input);
        assert_eq!(text(&app), "aXYb");
    }

    #[test]
    fn edit_menu_availability_follows_mode_selection_and_history() {
        let ctx = ctx_with_fonts();
        let mut app = app_with_text("some text");
        let _ = editor_frame(&ctx, &mut app);
        let can = app.edit_availability(&ctx);
        assert!(can.editor && can.text && !can.selection && !can.undo && !can.redo);
        assert!(!can.allows(&AppCommand::Copy));
        assert!(can.allows(&AppCommand::Paste));

        app.select_in_editor(&ctx, 0..4);
        let can = app.edit_availability(&ctx);
        assert!(can.allows(&AppCommand::Cut) && can.allows(&AppCommand::Copy));

        app.set_mode(Mode::Preview, &ctx);
        let can = app.edit_availability(&ctx);
        assert_eq!(can, EditAvailability::default());
        for cmd in [AppCommand::Cut, AppCommand::Paste, AppCommand::Undo] {
            assert!(!can.allows(&cmd), "{cmd:?} greyed in Preview");
        }
        // Commands are ignored without an editor.
        app.execute(AppCommand::Cut, &ctx);
        app.execute(AppCommand::FindNext, &ctx);
        app.execute(AppCommand::GoToLine, &ctx);
        assert!(app.editor_ops.pending.is_empty());
        assert!(app.goto_dialog.is_none());
    }

    #[test]
    fn find_next_and_previous_select_matches_and_wrap() {
        let ctx = ctx_with_fonts();
        let mut app = app_with_text("foo bär foo baz");
        app.execute(AppCommand::FindNext, &ctx);
        assert!(app.search.visible, "no query: opens the find bar");

        app.search.query = "foo".to_owned();
        app.execute(AppCommand::FindNext, &ctx);
        assert_eq!(app.editor_selection(&ctx), 0..3);
        assert!(app.focus_editor && app.editor_ops.scroll_to_cursor);
        app.execute(AppCommand::FindNext, &ctx);
        assert_eq!(app.editor_selection(&ctx), 8..11, "char indices past 'ä'");
        app.execute(AppCommand::FindNext, &ctx);
        assert_eq!(app.editor_selection(&ctx), 0..3, "wraps to the start");
        app.execute(AppCommand::FindPrevious, &ctx);
        assert_eq!(app.editor_selection(&ctx), 8..11, "wraps to the end");

        app.search.query = "zzz".to_owned();
        app.execute(AppCommand::FindNext, &ctx);
        assert_eq!(app.editor_selection(&ctx), 8..11, "no match: unchanged");

        // F3 while typing in the find bar keeps focus there.
        app.search.query = "foo".to_owned();
        app.focus_editor = false;
        ctx.memory_mut(|m| m.request_focus(egui::Id::new(FIND_QUERY_ID)));
        app.execute(AppCommand::FindNext, &ctx);
        assert_eq!(app.editor_selection(&ctx), 0..3);
        assert!(!app.focus_editor, "focus stays in the find bar");
    }

    #[test]
    fn unfocused_selection_is_painted() {
        let ctx = ctx_with_fonts();
        let mut app = app_with_text("alpha beta\ngamma");
        let shapes = |app: &mut RustdownApp| {
            let out = editor_frame(&ctx, app);
            out.shapes
                .iter()
                .filter(|clipped| matches!(clipped.shape, egui::Shape::Rect(_)))
                .count()
        };
        let _ = editor_frame(&ctx, &mut app);
        let plain = shapes(&mut app);
        app.select_in_editor(&ctx, 6..13); // "beta\nga": two rows
        app.focus_editor = false;
        let selected = shapes(&mut app);
        assert!(selected >= plain + 2, "{plain} -> {selected}");
    }

    #[test]
    fn go_to_line_moves_caret_and_scrolls_into_view() {
        let ctx = ctx_with_fonts();
        let body = (1..=300).fold(String::new(), |mut body, n| {
            body.push_str("line ");
            body.push_str(&n.to_string());
            body.push('\n');
            body
        });
        let mut app = app_with_text(&body);
        let _ = editor_frame(&ctx, &mut app);

        app.execute(AppCommand::GoToLine, &ctx);
        assert_eq!(
            app.goto_dialog.as_ref().map(|d| d.input.as_str()),
            Some("1"),
            "prefilled with the caret line"
        );
        app.goto_dialog = None;

        app.go_to_line(&ctx, 200);
        let caret = app.editor_selection(&ctx);
        assert_eq!(
            caret,
            line_start_char(&body, 200)..line_start_char(&body, 200)
        );
        assert_eq!(line_col(&body, caret.start), (200, 1));
        let _ = editor_frame(&ctx, &mut app);
        let _ = editor_frame(&ctx, &mut app);
        // Line 200 is centred in the 768 px viewport, without animation.
        app.tab_mut().ensure_row_byte_offsets();
        let line_y = app.tab().editor_byte_to_y(char_to_byte(&body, caret.start));
        let scrolled = app.tab_mut().current_editor_scroll_y(&ctx);
        assert!(
            matches!((line_y, scrolled), (Some(line), Some(y)) if y > line - 768.0 && y < line),
            "{line_y:?} {scrolled:?}"
        );
        assert!(ctx.memory(|m| m.has_focus(app.tab().editor_id())));

        app.go_to_line(&ctx, 10_000);
        assert_eq!(
            app.editor_selection(&ctx).start,
            line_start_char(&body, 301)
        );
    }

    #[test]
    fn word_wrap_off_keeps_one_row_per_line() {
        let ctx = ctx_with_fonts();
        let long_line = "word ".repeat(400);
        let mut app = app_with_text(&format!("{long_line}\nshort\n"));
        let rows = |app: &RustdownApp| {
            app.tab()
                .doc
                .editor_galley_cache
                .as_ref()
                .map(|cache| (cache.galley.rows.len(), cache.galley.size().x))
        };

        let _ = editor_frame(&ctx, &mut app);
        let wrapped = rows(&app);
        assert!(wrapped.is_some_and(|(rows, _)| rows > 3), "{wrapped:?}");

        app.word_wrap = false;
        let _ = editor_frame(&ctx, &mut app);
        let unwrapped = rows(&app);
        assert!(
            unwrapped.is_some_and(|(rows, width)| rows == 3 && width > 1024.0),
            "{unwrapped:?}"
        );
        // Scroll ↔ byte mapping still works without wrapping.
        let _ = editor_frame(&ctx, &mut app);
        assert_eq!(
            app.tab_mut().current_scroll_byte_offset(&ctx, Mode::Edit),
            Some(0)
        );
    }

    #[test]
    fn save_all_saves_every_dirty_file_tab() {
        let dir = std::env::temp_dir().join(format!("rustdown-save-all-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut tabs = Vec::new();
        for name in ["a.md", "b.md", "c.md"] {
            let path = dir.join(name);
            let _ = std::fs::write(&path, "old\n");
            let Ok(tab) = crate::tab::Tab::from_path(path) else {
                unreachable!()
            };
            tabs.push(tab);
        }
        let mut app = RustdownApp {
            tabs,
            ..RustdownApp::default()
        };
        for index in [0, 2] {
            app.tabs[index].doc.text = Arc::new("new\n".to_owned());
            app.tabs[index].note_text_changed(false);
        }
        app.execute(AppCommand::SaveAll, &egui::Context::default());
        for (name, expected) in [("a.md", "new\n"), ("b.md", "old\n"), ("c.md", "new\n")] {
            assert_eq!(
                std::fs::read_to_string(dir.join(name)).ok().as_deref(),
                Some(expected),
                "{name}"
            );
        }
        assert!(app.tabs.iter().all(|tab| !tab.doc.dirty));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
