//! View → Spell check: misspelled words in the editor get a red wavy line;
//! right-click one for suggestions, Add to dictionary or Ignore.
//!
//! Windows' own checker does the work (`win_spell`, offline, with the
//! user's dictionary), on a worker thread, for the active tab, a moment
//! after typing stops.  Only prose is checked: not code, HTML, link targets
//! or words that look like addresses, paths or identifiers.  Between
//! checks the marks before and after the edited part move with the text;
//! the ones in it go until the next check.

use std::{
    collections::HashMap,
    ops::Range,
    sync::{Arc, mpsc},
    time::Duration,
};

use eframe::egui;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::{RustdownApp, commands::AppCommand, menu, tab::TabId};

/// Wait after the last keystroke before checking.
const CHECK_DELAY: Duration = Duration::from_millis(500);
/// Most misspellings marked in one document.
const MAX_MARKED: usize = 2000;
/// Horizontal step of the wavy line, in points.
const WAVE_STEP: f32 = 2.0;
/// Height of the wavy line, in points.
const WAVE_HEIGHT: f32 = 1.5;

/// One spelling error from a [`Checker`]: UTF-16 offsets into the text
/// checked.
pub(crate) struct Found {
    pub(crate) start: usize,
    pub(crate) len: usize,
    /// A repeated word ("the the"), to delete.
    pub(crate) repeated: bool,
}

/// A spell checker: Windows' in the app, a word list in tests.
pub(crate) trait Checker {
    /// The errors in `text` (UTF-16, NUL-terminated).
    fn check(&self, text: &[u16]) -> Vec<Found>;
    /// Suggested spellings of `word`, best first.
    fn suggest(&self, word: &str) -> Vec<String>;
    /// Add `word` to the user's dictionary.
    fn add(&self, word: &str);
    /// Stop marking `word` until the app closes.
    fn ignore(&self, word: &str);
}

/// A misspelled (or repeated) word of a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Misspelling {
    pub(crate) bytes: Range<usize>,
    pub(crate) repeated: bool,
}

/// The parts of `text` to check, as byte ranges: the text of paragraphs,
/// headings, lists, quotes and tables (not code or HTML), joined where
/// only white space (a line break) separates them.
#[must_use]
pub(crate) fn prose_ranges(text: &str) -> Vec<Range<usize>> {
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_GFM;
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut code_blocks = 0usize;
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_blocks += 1,
            Event::End(TagEnd::CodeBlock) => code_blocks = code_blocks.saturating_sub(1),
            Event::Text(_) if code_blocks == 0 && !range.is_empty() => {
                if let Some(last) = ranges.last_mut()
                    && last.end <= range.start
                    && text[last.end..range.start].trim().is_empty()
                {
                    last.end = range.end;
                } else {
                    ranges.push(range);
                }
            }
            _ => {}
        }
    }
    ranges
}

/// Whether a word (a run of non-space characters) is an address, a path
/// or an identifier rather than prose.
fn looks_like_code(word: &str) -> bool {
    word.contains("://") || word.starts_with("www.") || word.contains(['@', '/', '\\', '_', '='])
}

/// `text` split into runs of white space and runs of other characters,
/// with their byte offsets.
fn runs(text: &str) -> impl Iterator<Item = (usize, &str)> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        let rest = text.get(start..)?;
        let space = rest.chars().next()?.is_whitespace();
        let len = rest
            .find(|c: char| c.is_whitespace() != space)
            .unwrap_or(rest.len());
        let run = (start, &rest[..len]);
        start += len;
        Some(run)
    })
}

/// `segment` (at byte `base` of the document) as NUL-terminated UTF-16 for
/// the checker, with code-like words blanked; and the document byte of
/// each UTF-16 unit (plus one for the end).
fn utf16_with_offsets(segment: &str, base: usize) -> (Vec<u16>, Vec<usize>) {
    let mut units = Vec::with_capacity(segment.len() + 1);
    let mut offsets = Vec::with_capacity(segment.len() + 1);
    for (at, run) in runs(segment) {
        let blank = !run.starts_with(char::is_whitespace) && looks_like_code(run);
        for (index, c) in run.char_indices() {
            let mut buffer = [0u16; 2];
            for &unit in c.encode_utf16(&mut buffer).iter() {
                units.push(if blank { u16::from(b' ') } else { unit });
                offsets.push(base + at + index);
            }
        }
    }
    offsets.push(base + segment.len());
    units.push(0);
    (units, offsets)
}

/// The misspellings in `text`, in order.
#[must_use]
pub(crate) fn find_misspellings(text: &str, checker: &dyn Checker) -> Vec<Misspelling> {
    let mut out = Vec::new();
    for range in prose_ranges(text) {
        let (units, offsets) = utf16_with_offsets(&text[range.clone()], range.start);
        for found in checker.check(&units) {
            let (Some(&start), Some(&end)) = (
                offsets.get(found.start),
                offsets.get(found.start + found.len),
            ) else {
                continue;
            };
            if start < end {
                out.push(Misspelling {
                    bytes: start..end,
                    repeated: found.repeated,
                });
            }
            if out.len() >= MAX_MARKED {
                return out;
            }
        }
    }
    out
}

/// `misspellings` of `old` moved onto `new` (the text after some edits):
/// the ones wholly before or after the changed part stay (shifted), the
/// ones touching it go.
#[must_use]
pub(crate) fn carry_over(old: &str, new: &str, misspellings: &[Misspelling]) -> Vec<Misspelling> {
    if old == new {
        return misspellings.to_vec();
    }
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !new.is_char_boundary(prefix) || !old.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let most = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(most)
        .take_while(|(x, y)| x == y)
        .count();
    while !old.is_char_boundary(a.len() - suffix) || !new.is_char_boundary(b.len() - suffix) {
        suffix -= 1;
    }
    let old_tail = a.len() - suffix;
    let new_tail = b.len() - suffix;
    misspellings
        .iter()
        .filter_map(|misspelling| {
            let Range { start, end } = misspelling.bytes;
            if end < prefix {
                Some(misspelling.clone())
            } else if start > old_tail {
                Some(Misspelling {
                    bytes: start - old_tail + new_tail..end - old_tail + new_tail,
                    repeated: misspelling.repeated,
                })
            } else {
                None
            }
        })
        .collect()
}

/// The char ranges of `misspellings` (sorted byte ranges) in `text`.
#[must_use]
pub(crate) fn char_ranges(text: &str, misspellings: &[Misspelling]) -> Vec<Range<usize>> {
    let mut out = Vec::with_capacity(misspellings.len());
    let (mut byte, mut chars) = (0, 0);
    let mut to_char = |target: usize| {
        let target = target.min(text.len());
        if target >= byte {
            chars += text
                .get(byte..target)
                .map_or(0, |part| part.chars().count());
        } else {
            chars = text.get(..target).map_or(0, |part| part.chars().count());
        }
        byte = target;
        chars
    };
    for misspelling in misspellings {
        let start = to_char(misspelling.bytes.start);
        let end = to_char(misspelling.bytes.end);
        out.push(start..end);
    }
    out
}

/// Work for the checker thread.
enum Request {
    Check { tab: TabId, text: Arc<String> },
    Suggest(String),
    Add(String),
    Ignore(String),
}

/// What the checker thread sends back.
enum Reply {
    Checked {
        tab: TabId,
        text: Arc<String>,
        misspellings: Vec<Misspelling>,
    },
    Suggestions {
        word: String,
        list: Vec<String>,
    },
    /// This PC has no spell checker for the user's language (or this is
    /// not Windows).
    Unavailable,
}

#[cfg(windows)]
fn make_checker() -> Option<Box<dyn Checker>> {
    crate::win_spell::WinSpellChecker::new().map(|checker| Box::new(checker) as Box<dyn Checker>)
}

#[cfg(not(windows))]
fn make_checker() -> Option<Box<dyn Checker>> {
    None
}

/// The checker thread: answers requests until the app drops its sender.
/// Of several checks of one tab waiting, only the newest runs.
fn run_worker(
    requests: &mpsc::Receiver<Request>,
    replies: &mpsc::Sender<Reply>,
    ctx: &egui::Context,
) {
    let Some(checker) = make_checker() else {
        let _ = replies.send(Reply::Unavailable);
        ctx.request_repaint();
        return;
    };
    while let Ok(first) = requests.recv() {
        let mut batch = vec![first];
        batch.extend(requests.try_iter());
        let mut newest: HashMap<TabId, usize> = HashMap::new();
        for (index, request) in batch.iter().enumerate() {
            if let Request::Check { tab, .. } = request {
                newest.insert(*tab, index);
            }
        }
        for (index, request) in batch.into_iter().enumerate() {
            let reply = match request {
                Request::Check { tab, text } => {
                    if newest.get(&tab) != Some(&index) {
                        continue;
                    }
                    let misspellings = find_misspellings(&text, checker.as_ref());
                    Reply::Checked {
                        tab,
                        text,
                        misspellings,
                    }
                }
                Request::Suggest(word) => {
                    let list = checker.suggest(&word);
                    Reply::Suggestions { word, list }
                }
                Request::Add(word) => {
                    checker.add(&word);
                    continue;
                }
                Request::Ignore(word) => {
                    checker.ignore(&word);
                    continue;
                }
            };
            if replies.send(reply).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    }
}

/// The misspelling a right-click in the editor landed on.
#[derive(Clone, Debug)]
struct MenuTarget {
    tab: TabId,
    bytes: Range<usize>,
    word: String,
    repeated: bool,
}

/// What the editor's context menu asked for.
enum SpellAction {
    Replace(String),
    Add,
    Ignore,
}

/// App-wide spell-check state.
#[derive(Default)]
pub(crate) struct SpellCheck {
    /// View → Spell check (persisted).
    pub(crate) enabled: bool,
    /// The checker thread's channels, once started.
    worker: Option<(mpsc::Sender<Request>, mpsc::Receiver<Reply>)>,
    /// No checker on this PC.
    unavailable: bool,
    /// Suggestions by word, as they arrive.
    suggestions: HashMap<String, Vec<String>>,
    /// The misspelling the editor's context menu is about.
    menu_target: Option<MenuTarget>,
}

impl SpellCheck {
    /// Whether misspellings are marked now.
    #[must_use]
    pub(crate) const fn active(&self) -> bool {
        self.enabled && !self.unavailable
    }
}

/// A tab's spell-check state.
#[derive(Debug, Default)]
pub(crate) struct TabSpelling {
    /// Edit sequence of the text last sent to be checked.
    requested: Option<u64>,
    /// The text last checked, and its misspellings.
    checked_text: Arc<String>,
    misspellings: Vec<Misspelling>,
    /// Edit sequence `shown` and `shown_chars` were made for.
    shown_seq: Option<u64>,
    shown: Vec<Misspelling>,
    /// The char ranges of `shown`, for painting.
    pub(crate) shown_chars: Vec<Range<usize>>,
}

impl TabSpelling {
    /// Move the last check's misspellings onto `text` (edit sequence
    /// `seq`), unless done already.
    pub(crate) fn refresh(&mut self, text: &str, seq: u64) {
        if self.shown_seq != Some(seq) {
            self.shown = carry_over(&self.checked_text, text, &self.misspellings);
            self.shown_chars = char_ranges(text, &self.shown);
            self.shown_seq = Some(seq);
        }
    }

    /// Whether the marks were made for edit sequence `seq`.
    #[must_use]
    pub(crate) fn is_for(&self, seq: u64) -> bool {
        self.shown_seq == Some(seq)
    }
}

/// Paint a wavy line under each of `chars` (sorted char ranges) of
/// `galley`, drawn at `origin`; only on the rows inside the clip rect.
pub(crate) fn paint_squiggles(
    painter: &egui::Painter,
    galley: &egui::Galley,
    origin: egui::Pos2,
    chars: &[Range<usize>],
    color: egui::Color32,
) {
    if chars.is_empty() {
        return;
    }
    let clip = painter.clip_rect();
    let mut row_start = 0usize;
    let mut next = 0usize;
    for row in &galley.rows {
        let row_end = row_start + row.char_count_including_newline();
        let (top, bottom) = (origin.y + row.min_y(), origin.y + row.max_y());
        if top > clip.bottom() {
            break;
        }
        while next < chars.len() && chars[next].end <= row_start {
            next += 1;
        }
        if bottom >= clip.top() {
            for range in chars[next..]
                .iter()
                .take_while(|range| range.start < row_end)
            {
                let from = range.start.max(row_start) - row_start;
                let to = range.end.min(row_end) - row_start;
                let x0 = origin.x + row.pos.x + row.x_offset(from);
                let x1 = origin.x + row.pos.x + row.x_offset(to);
                wave(painter, x0, x1, bottom - 1.0, color);
            }
        }
        row_start = row_end;
    }
}

/// A wavy line from `x0` to `x1` with its low points at `y`.
fn wave(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: egui::Color32) {
    if x1 - x0 < 1.0 {
        return;
    }
    let steps = ((x1 - x0) / WAVE_STEP).ceil() as usize;
    let points = (0..=steps)
        .map(|step| {
            let x = (step as f32).mul_add(WAVE_STEP, x0).min(x1);
            let y = if step % 2 == 0 { y } else { y - WAVE_HEIGHT };
            egui::pos2(x, y)
        })
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.0, color)));
}

impl RustdownApp {
    /// Each frame: take the checker's answers, and ask it to check the
    /// active tab when its text changed and typing has paused.
    pub(crate) fn tick_spelling(&mut self, ctx: &egui::Context) {
        self.take_spelling_replies(ctx);
        if !self.spell.active() || !self.mode.uses_editor() {
            return;
        }
        let tab = self.tab();
        let seq = tab.doc.edit_seq;
        if tab.spelling.requested == Some(seq)
            || tab.doc.text.len() > crate::live_format::MAX_FORMATTED_BYTES
        {
            return;
        }
        if let Some(wait) = tab.doc.debounce_remaining(CHECK_DELAY) {
            ctx.request_repaint_after(wait);
            return;
        }
        let request = Request::Check {
            tab: tab.id,
            text: tab.doc.text.clone(),
        };
        if self.send_spelling(ctx, request) {
            self.tab_mut().spelling.requested = Some(seq);
        }
    }

    fn take_spelling_replies(&mut self, ctx: &egui::Context) {
        let replies: Vec<Reply> = match &self.spell.worker {
            Some((_, replies)) => replies.try_iter().collect(),
            None => return,
        };
        for reply in replies {
            match reply {
                Reply::Checked {
                    tab,
                    text,
                    misspellings,
                } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|candidate| candidate.id == tab) {
                        tab.spelling.checked_text = text;
                        tab.spelling.misspellings = misspellings;
                        tab.spelling.shown_seq = None;
                    }
                }
                Reply::Suggestions { word, list } => {
                    self.spell.suggestions.insert(word, list);
                }
                Reply::Unavailable => {
                    self.spell.unavailable = true;
                    self.spell.worker = None;
                    if self.spell.enabled {
                        self.notify(
                            ctx,
                            "Spell check isn't available: Windows has no spell checker for \
                             your language.",
                        );
                    }
                }
            }
        }
    }

    /// Send `request` to the checker thread (started on first use).
    fn send_spelling(&mut self, ctx: &egui::Context, request: Request) -> bool {
        if self.spell.worker.is_none() {
            let (request_tx, request_rx) = mpsc::channel();
            let (reply_tx, reply_rx) = mpsc::channel();
            let repaint = ctx.clone();
            let spawned = std::thread::Builder::new()
                .name("spell-check".to_owned())
                .spawn(move || run_worker(&request_rx, &reply_tx, &repaint));
            if spawned.is_err() {
                self.spell.unavailable = true;
                return false;
            }
            self.spell.worker = Some((request_tx, reply_rx));
        }
        self.spell
            .worker
            .as_ref()
            .is_some_and(|(requests, _)| requests.send(request).is_ok())
    }

    /// View → Spell check.
    pub(crate) fn toggle_spell_check(&mut self, ctx: &egui::Context) {
        self.spell.enabled = !self.spell.enabled;
        if self.spell.enabled && self.spell.unavailable {
            self.notify(
                ctx,
                "Spell check isn't available: Windows has no spell checker for your language.",
            );
        }
        self.save_preferences();
    }

    /// The misspelling of the active tab at char `char_index`.
    fn misspelling_at(&self, char_index: usize) -> Option<MenuTarget> {
        let tab = self.tab();
        let spelling = &tab.spelling;
        if !spelling.is_for(tab.doc.edit_seq) {
            return None;
        }
        let at = spelling
            .shown_chars
            .iter()
            .position(|chars| chars.start <= char_index && char_index <= chars.end)?;
        let misspelling = spelling.shown.get(at)?;
        Some(MenuTarget {
            tab: tab.id,
            bytes: misspelling.bytes.clone(),
            word: tab.doc.text.get(misspelling.bytes.clone())?.to_owned(),
            repeated: misspelling.repeated,
        })
    }

    /// The editor's right-click menu: spelling suggestions when the click
    /// was on a misspelled word (`clicked_char`, on the frame of the
    /// click), the table rows (`table_edit`) when it was in a table, then
    /// Cut, Copy, Paste and Select all.
    pub(crate) fn editor_context_menu(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        clicked_char: Option<usize>,
    ) {
        if let Some(index) = clicked_char {
            self.spell.menu_target = if self.spell.active() {
                self.misspelling_at(index)
            } else {
                None
            };
            if let Some(target) = &self.spell.menu_target
                && !target.repeated
                && !self.spell.suggestions.contains_key(&target.word)
            {
                let word = target.word.clone();
                self.send_spelling(ctx, Request::Suggest(word));
            }
            let text = self.tab().doc.text.clone();
            let byte = crate::edit_ops::char_to_byte(&text, index);
            self.table_menu = crate::table_edit::find(&text, byte).map(|table| (byte, table));
        }
        let table_menu = self.table_menu.clone();
        let mut table_op = None;
        let target = self.spell.menu_target.clone();
        let suggestions = target
            .as_ref()
            .and_then(|target| self.spell.suggestions.get(&target.word).cloned());
        let mut chosen = None;
        menu::context_menu(response, |ui| {
            menu::hide_lead_column(ui);
            if let Some(target) = &target {
                if target.repeated {
                    if menu::plain_row(ui, "Delete repeated word", menu::Mark::None) {
                        chosen = Some(SpellAction::Replace(String::new()));
                    }
                } else {
                    match &suggestions {
                        Some(list) if !list.is_empty() => {
                            for word in list {
                                if menu::plain_row(ui, word, menu::Mark::None) {
                                    chosen = Some(SpellAction::Replace(word.clone()));
                                }
                            }
                        }
                        found => {
                            let note = if found.is_some() {
                                "No suggestions"
                            } else {
                                "Looking for suggestions…"
                            };
                            ui.add_enabled_ui(false, |ui| {
                                let _ = menu::plain_row(ui, note, menu::Mark::None);
                            });
                        }
                    }
                    menu::separator(ui);
                    if menu::plain_row(ui, "Add to dictionary", menu::Mark::None) {
                        chosen = Some(SpellAction::Add);
                    }
                    if menu::plain_row(ui, "Ignore", menu::Mark::None) {
                        chosen = Some(SpellAction::Ignore);
                    }
                }
                menu::separator(ui);
            }
            if let Some((byte, table)) = &table_menu {
                let (line, _) = table.cell_at(*byte);
                if let Some(op) = crate::table_edit::menu_section(ui, table, line) {
                    table_op = Some(op);
                }
                menu::separator(ui);
            }
            self.menu_item(ui, "Cut", AppCommand::Cut);
            self.menu_item(ui, "Copy", AppCommand::Copy);
            self.menu_item(ui, "Paste", AppCommand::Paste);
            menu::separator(ui);
            self.menu_item(ui, "Select all", AppCommand::SelectAll);
        });
        if let (Some(op), Some((byte, _))) = (table_op, table_menu) {
            self.table_menu = None;
            self.apply_table_op(ctx, byte, op);
        }
        let (Some(action), Some(target)) = (chosen, target) else {
            return;
        };
        self.spell.menu_target = None;
        match action {
            SpellAction::Replace(with) => self.replace_misspelling(ctx, &target, &with),
            SpellAction::Add => self.forget_misspelling(ctx, Request::Add(target.word)),
            SpellAction::Ignore => self.forget_misspelling(ctx, Request::Ignore(target.word)),
        }
    }

    /// Replace the misspelled word with `with` (empty: delete a repeated
    /// word and the space before it), as one undoable edit.
    fn replace_misspelling(&mut self, ctx: &egui::Context, target: &MenuTarget, with: &str) {
        let tab = self.tab();
        if tab.id != target.tab
            || tab.doc.text.get(target.bytes.clone()) != Some(target.word.as_str())
        {
            return;
        }
        let mut range = target.bytes.clone();
        if with.is_empty() && tab.doc.text[..range.start].ends_with(' ') {
            range.start -= 1;
        }
        let caret = range.start + with.len();
        self.replace_text_undoable(ctx, range, with, caret..caret);
    }

    /// Add to the dictionary or ignore a word (`request`): its marks go
    /// from every tab now, and the tabs are checked again.
    fn forget_misspelling(&mut self, ctx: &egui::Context, request: Request) {
        let (Request::Add(word) | Request::Ignore(word)) = &request else {
            return;
        };
        let word = word.clone();
        for tab in &mut self.tabs {
            let spelling = &mut tab.spelling;
            let checked = spelling.checked_text.clone();
            spelling.misspellings.retain(|misspelling| {
                checked.get(misspelling.bytes.clone()) != Some(word.as_str())
            });
            spelling.shown_seq = None;
            spelling.requested = None;
        }
        self.send_spelling(ctx, request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flags the words of a list (and "the the").
    struct WordList(&'static [&'static str]);

    impl Checker for WordList {
        fn check(&self, text: &[u16]) -> Vec<Found> {
            let text = String::from_utf16_lossy(&text[..text.len() - 1]);
            let mut found = Vec::new();
            let mut previous = "";
            for (at, run) in runs(&text) {
                if run.starts_with(char::is_whitespace) {
                    continue;
                }
                let word = run.trim_matches(|c: char| !c.is_alphanumeric());
                let start = text[..at].encode_utf16().count();
                let len = word.encode_utf16().count();
                if self.0.contains(&word) || word == previous {
                    found.push(Found {
                        start,
                        len,
                        repeated: word == previous,
                    });
                }
                previous = word;
            }
            found
        }
        fn suggest(&self, _word: &str) -> Vec<String> {
            Vec::new()
        }
        fn add(&self, _word: &str) {}
        fn ignore(&self, _word: &str) {}
    }

    fn words(text: &str, misspellings: &[Misspelling]) -> Vec<String> {
        misspellings
            .iter()
            .map(|misspelling| text[misspelling.bytes.clone()].to_owned())
            .collect()
    }

    #[test]
    fn only_prose_is_checked() {
        let text = "# Teh title\n\nSome teh text `teh code` and [teh link](http://teh.example).\n\n\
                    ```\nteh\n```\n\n<div>\nteh\n</div>\n\nSee www.teh.com or teh_thing, café teh.\n";
        let found = find_misspellings(text, &WordList(&["teh", "Teh"]));
        assert_eq!(words(text, &found), ["Teh", "teh", "teh", "teh"]);
        // The last one is after a multi-byte char: byte offsets still right.
        assert_eq!(found.last().map(|m| m.bytes.end), Some(text.len() - 2));
    }

    #[test]
    fn repeated_words_are_found_across_line_breaks() {
        let text = "go to the\nthe shop\n";
        let found = find_misspellings(text, &WordList(&[]));
        assert_eq!(found.len(), 1);
        assert!(found[0].repeated);
        assert_eq!(&text[found[0].bytes.clone()], "the");
    }

    #[test]
    fn marks_move_with_edits_around_them() {
        let old = "aa teh bb teh cc";
        let marks = [
            Misspelling {
                bytes: 3..6,
                repeated: false,
            },
            Misspelling {
                bytes: 10..13,
                repeated: false,
            },
        ];
        // Typing between them: the first stays, the second moves.
        let new = "aa teh bXXb teh cc";
        let moved = carry_over(old, new, &marks);
        assert_eq!(words(new, &moved), ["teh", "teh"]);
        // Typing in a marked word drops its mark.
        let new = "aa tehh bb teh cc";
        assert_eq!(words(new, &carry_over(old, new, &marks)), ["teh"]);
        // Char ranges count chars, not bytes.
        let text = "é teh";
        let marks = [Misspelling {
            bytes: 3..6,
            repeated: false,
        }];
        assert_eq!(char_ranges(text, &marks).first(), Some(&(2..5)));
    }
}
