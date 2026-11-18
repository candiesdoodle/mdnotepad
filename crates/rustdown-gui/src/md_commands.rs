//! Markdown formatting commands as pure text transforms.
//!
//! A command takes the document text and the editor selection (char indices,
//! as egui's `CCursorRange` uses) and returns an [`Edit`]: the char range to
//! replace, the text to insert there, and the selection to show afterwards.
//! Nothing here depends on egui, so every transform is unit-tested directly.
//!
//! * **Line endings**: inserted line breaks use the document's style (the
//!   caller passes it in).  A selection boundary between `\r` and `\n` is
//!   moved before the `\r`, so a pair is never split.
//! * **Touched lines**: every line the selection overlaps.  A selection that
//!   ends right after a line break does not touch the next line.
//! * **Toggles**: a command is removed when every touched line (or selected
//!   span) already has it, and applied otherwise.

use std::ops::Range;

use crate::line_ending::{EndOfLine, inside_crlf};

/// A Markdown formatting command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MdCommand {
    /// Toggle `**bold**`.
    Bold,
    /// Toggle `*italic*`.
    Italic,
    /// Toggle `~~strikethrough~~`.
    Strikethrough,
    /// Toggle `` `inline code` ``.
    InlineCode,
    /// Insert `[text](url)`.
    Link,
    /// Toggle an ATX heading; levels outside 1–6 are clamped.
    Heading(u8),
    /// Remove headings.
    Paragraph,
    /// Toggle `- ` list items.
    BulletList,
    /// Toggle `1. ` list items, numbered consecutively.
    NumberedList,
    /// Toggle `- [ ] ` task items.
    TaskList,
    /// Toggle `> ` quote lines.
    Blockquote,
    /// Toggle a fenced code block around the touched lines.
    CodeBlock,
    /// Insert a `---` line.
    HorizontalRule,
    /// Insert a two-column table template.
    Table,
}

/// The result of a command.  All indices are char indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    /// Chars of the original text to replace.
    pub replace: Range<usize>,
    /// Text inserted in place of `replace`.
    pub insert: String,
    /// Selection in the edited text (`start == end` for a caret).
    pub selection: Range<usize>,
}

impl Edit {
    /// `true` when the edit leaves the text unchanged.
    #[must_use]
    pub const fn is_noop(&self) -> bool {
        self.replace.start == self.replace.end && self.insert.is_empty()
    }

    /// Apply the edit to `text`, the text it was computed from.
    pub fn apply_to(&self, text: &mut String) {
        let start = char_to_byte(text, self.replace.start);
        let end = start + char_to_byte(&text[start..], self.replace.len());
        text.replace_range(start..end, &self.insert);
    }
}

/// Run `cmd` on `text` with `selection` (char indices, in either order);
/// line breaks it adds are `eol`.
#[must_use]
pub fn apply(cmd: MdCommand, text: &str, selection: Range<usize>, eol: EndOfLine) -> Edit {
    let eol = eol.as_str();
    let sel = byte_selection(text, selection);
    let change = match cmd {
        MdCommand::Bold => inline_toggle(text, &sel, Inline::Bold),
        MdCommand::Italic => inline_toggle(text, &sel, Inline::Italic),
        MdCommand::Strikethrough => inline_toggle(text, &sel, Inline::Strike),
        MdCommand::InlineCode => inline_toggle(text, &sel, Inline::Code),
        MdCommand::Link => link(text, &sel),
        MdCommand::Heading(_)
        | MdCommand::Paragraph
        | MdCommand::BulletList
        | MdCommand::NumberedList
        | MdCommand::TaskList
        | MdCommand::Blockquote => line_prefix_toggle(text, &sel, cmd),
        MdCommand::CodeBlock => code_block(text, &sel, eol),
        MdCommand::HorizontalRule => {
            let (splice, block_start) = insert_block(text, &sel, &["---"], eol);
            let caret = block_start + "---".len() + eol.len();
            Change {
                splices: vec![splice],
                selection: caret..caret,
            }
        }
        MdCommand::Table => {
            let (splice, block_start) = insert_block(text, &sel, &TABLE_TEMPLATE, eol);
            let cell = block_start + "| ".len();
            Change {
                splices: vec![splice],
                selection: cell..cell + "Column 1".len(),
            }
        }
    };
    change.into_edit(text)
}

/// Heading level shared by every non-blank line the selection touches
/// (`0` = not a heading), or `None` when the lines differ.
#[must_use]
pub fn heading_level(text: &str, selection: Range<usize>) -> Option<u8> {
    let sel = byte_selection(text, selection);
    let mut levels = touched_lines(text, &sel)
        .filter(|line| !line.is_blank(text))
        .map(|line| parse_prefix(line.content(text)).heading);
    let first = levels.next().unwrap_or(0);
    levels.all(|level| level == first).then_some(first)
}

const TABLE_TEMPLATE: [&str; 4] = [
    "| Column 1 | Column 2 |",
    "| -------- | -------- |",
    "| Text     | Text     |",
    "| Text     | Text     |",
];

// ── Char / byte conversion ──────────────────────────────────────────────

/// Byte offset of char `char_index` (clamped to the end of `text`).
fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte, _)| byte)
}

/// Char index of byte offset `byte` (a char boundary).
fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

/// Sorted byte range of a char selection, with both ends moved out of any
/// `\r\n` pair.
fn byte_selection(text: &str, selection: Range<usize>) -> Range<usize> {
    let (a, b) = if selection.start <= selection.end {
        (selection.start, selection.end)
    } else {
        (selection.end, selection.start)
    };
    let start = snap_out_of_crlf(text, char_to_byte(text, a));
    let end = snap_out_of_crlf(text, char_to_byte(text, b)).max(start);
    start..end
}

fn snap_out_of_crlf(text: &str, pos: usize) -> usize {
    if inside_crlf(text, pos) { pos - 1 } else { pos }
}

// ── Changes ─────────────────────────────────────────────────────────────

/// Replace `range` (bytes of the original text) with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Splice {
    range: Range<usize>,
    text: String,
}

/// Sorted, non-overlapping splices plus the selection in the changed text
/// (bytes).
struct Change {
    splices: Vec<Splice>,
    selection: Range<usize>,
}

impl Change {
    /// Merge the splices into one minimal [`Edit`] in char indices.
    fn into_edit(self, text: &str) -> Edit {
        let region = match (self.splices.first(), self.splices.last()) {
            (Some(first), Some(last)) => first.range.start..last.range.end,
            _ => self.selection.start..self.selection.start,
        };
        let mut replacement = String::new();
        let mut pos = region.start;
        for splice in &self.splices {
            replacement.push_str(&text[pos..splice.range.start]);
            replacement.push_str(&splice.text);
            pos = splice.range.end;
        }

        let old = &text[region.clone()];
        let prefix = common_prefix_len(old, &replacement);
        let suffix = common_suffix_len(&old[prefix..], &replacement[prefix..]);
        let start = region.start + prefix;
        let end = region.end - suffix;
        let insert = replacement[prefix..replacement.len() - suffix].to_owned();

        let start_char = byte_to_char(text, start);
        let insert_chars = insert.chars().count();
        let to_char = |byte: usize| {
            if byte <= start {
                byte_to_char(text, byte)
            } else if byte <= start + insert.len() {
                start_char + insert[..byte - start].chars().count()
            } else {
                let tail = byte - start - insert.len();
                start_char + insert_chars + text[end..end + tail].chars().count()
            }
        };
        let selection = to_char(self.selection.start)..to_char(self.selection.end);
        Edit {
            replace: start_char..start_char + text[start..end].chars().count(),
            insert,
            selection,
        }
    }
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|&((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((i, _), _)| i)
}

fn common_suffix_len(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum()
}

/// Where byte `pos` of the original text ends up after `splices`.  A
/// position inside a replaced range moves to the end of the replacement
/// (`after`) or its start; an insertion exactly at `pos` lands before it when
/// `after` is set.
fn map_pos(splices: &[Splice], pos: usize, after: bool) -> usize {
    let (mut added, mut removed) = (0, 0);
    for splice in splices {
        let range = &splice.range;
        if pos < range.start || (pos == range.start && range.is_empty() && !after) {
            break;
        }
        if pos < range.end {
            let start = range.start + added - removed;
            return if after {
                start + splice.text.len()
            } else {
                start
            };
        }
        added += splice.text.len();
        removed += range.len();
    }
    pos + added - removed
}

// ── Lines ───────────────────────────────────────────────────────────────

/// One line: `start..end` is its content (without the line ending) and
/// `next` the start of the following line (`== end` on the last line when
/// it has no line ending).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Line {
    start: usize,
    end: usize,
    next: usize,
}

impl Line {
    const fn has_next(self) -> bool {
        self.next > self.end
    }

    fn content(self, text: &str) -> &str {
        &text[self.start..self.end]
    }

    fn is_blank(self, text: &str) -> bool {
        self.content(text).trim().is_empty()
    }
}

/// The line containing byte `pos` (a `\n` belongs to the line it ends).
fn line_at(text: &str, pos: usize) -> Line {
    let start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    match text[pos..].find('\n') {
        Some(i) => {
            let newline = pos + i;
            let end = if newline > start && text.as_bytes()[newline - 1] == b'\r' {
                newline - 1
            } else {
                newline
            };
            Line {
                start,
                end,
                next: newline + 1,
            }
        }
        None => Line {
            start,
            end: text.len(),
            next: text.len(),
        },
    }
}

fn is_line_start(text: &str, pos: usize) -> bool {
    pos == 0 || text.as_bytes()[pos - 1] == b'\n'
}

/// The lines `sel` touches, first to last.
fn touched_lines<'a>(text: &'a str, sel: &Range<usize>) -> impl Iterator<Item = Line> + 'a {
    let first = line_at(text, sel.start);
    let last_pos = if sel.end > sel.start && is_line_start(text, sel.end) {
        sel.end - 1
    } else {
        sel.end
    };
    std::iter::successors(Some(first), move |line| {
        (line.has_next() && line.next <= last_pos).then(|| line_at(text, line.next))
    })
}

/// The line before `line`, if any.
fn line_before(text: &str, line: Line) -> Option<Line> {
    (line.start > 0).then(|| line_at(text, line.start - 1))
}

/// The line after `line`, if any.
fn line_after(text: &str, line: Line) -> Option<Line> {
    line.has_next().then(|| line_at(text, line.next))
}

// ── Line prefixes ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListKind {
    Bullet,
    Numbered(u32),
    Task,
}

/// Block markers at the start of a line, as byte offsets into the line:
/// `> ` quote markers, indentation, a list marker, a heading marker, then
/// the content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Prefix {
    quote_end: usize,
    indent_end: usize,
    list: Option<ListKind>,
    list_end: usize,
    /// Heading level, `0` for none.
    heading: u8,
    content_start: usize,
}

fn parse_prefix(line: &str) -> Prefix {
    let bytes = line.as_bytes();
    let mut i = 0;
    loop {
        let spaces = bytes[i..]
            .iter()
            .take(3)
            .take_while(|&&b| b == b' ')
            .count();
        if bytes.get(i + spaces) != Some(&b'>') {
            break;
        }
        i = skip_one_space(bytes, i + spaces + 1);
    }
    let quote_end = i;
    while matches!(bytes.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    let indent_end = i;
    let (list, list_end) = parse_list_marker(line, indent_end);
    let (heading, content_start) = parse_heading_marker(bytes, list_end);
    Prefix {
        quote_end,
        indent_end,
        list,
        list_end,
        heading,
        content_start,
    }
}

fn is_space_or_end(bytes: &[u8], i: usize) -> bool {
    matches!(bytes.get(i), None | Some(b' ' | b'\t'))
}

fn skip_one_space(bytes: &[u8], i: usize) -> usize {
    if matches!(bytes.get(i), Some(b' ' | b'\t')) {
        i + 1
    } else {
        i
    }
}

fn parse_list_marker(line: &str, i: usize) -> (Option<ListKind>, usize) {
    let bytes = line.as_bytes();
    if matches!(bytes.get(i), Some(b'-' | b'*' | b'+')) && is_space_or_end(bytes, i + 1) {
        let after = skip_one_space(bytes, i + 1);
        let is_task = bytes.get(after) == Some(&b'[')
            && matches!(bytes.get(after + 1), Some(b' ' | b'x' | b'X'))
            && bytes.get(after + 2) == Some(&b']')
            && is_space_or_end(bytes, after + 3);
        if is_task {
            return (Some(ListKind::Task), skip_one_space(bytes, after + 3));
        }
        return (Some(ListKind::Bullet), after);
    }
    let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    if (1..=9).contains(&digits)
        && matches!(bytes.get(i + digits), Some(b'.' | b')'))
        && is_space_or_end(bytes, i + digits + 1)
    {
        let number = line[i..i + digits].parse().unwrap_or(1);
        return (
            Some(ListKind::Numbered(number)),
            skip_one_space(bytes, i + digits + 1),
        );
    }
    (None, i)
}

fn parse_heading_marker(bytes: &[u8], i: usize) -> (u8, usize) {
    let hashes = bytes[i..].iter().take_while(|&&b| b == b'#').count();
    if !(1..=6).contains(&hashes) || !is_space_or_end(bytes, i + hashes) {
        return (0, i);
    }
    let mut end = i + hashes;
    while matches!(bytes.get(end), Some(b' ' | b'\t')) {
        end += 1;
    }
    (hashes as u8, end)
}

/// Consecutive numbers per indentation level, for numbered lists.
struct Numbering {
    /// `(indent, next number)`, innermost last.
    levels: Vec<(usize, u32)>,
}

impl Numbering {
    /// Continue the numbering of a numbered item right above `first`.
    fn continuing(text: &str, first: Line) -> Self {
        let mut levels = Vec::new();
        if let Some(prev) = line_before(text, first) {
            let prefix = parse_prefix(prev.content(text));
            if let Some(ListKind::Numbered(n)) = prefix.list {
                levels.push((prefix.indent_end - prefix.quote_end, n.saturating_add(1)));
            }
        }
        Self { levels }
    }

    fn next(&mut self, indent: usize) -> u32 {
        while self.levels.last().is_some_and(|&(level, _)| level > indent) {
            self.levels.pop();
        }
        match self.levels.last_mut() {
            Some((level, next)) if *level == indent => {
                let number = *next;
                *next = next.saturating_add(1);
                number
            }
            _ => {
                self.levels.push((indent, 2));
                1
            }
        }
    }
}

fn is_list_kind(cmd: MdCommand, list: Option<ListKind>) -> bool {
    match cmd {
        MdCommand::BulletList => list == Some(ListKind::Bullet),
        MdCommand::TaskList => list == Some(ListKind::Task),
        MdCommand::NumberedList => matches!(list, Some(ListKind::Numbered(_))),
        _ => false,
    }
}

/// Headings, paragraph, lists and quotes: one prefix change per touched line.
fn line_prefix_toggle(text: &str, sel: &Range<usize>, cmd: MdCommand) -> Change {
    let lines: Vec<(Line, Prefix, bool)> = touched_lines(text, sel)
        .map(|line| (line, parse_prefix(line.content(text)), line.is_blank(text)))
        .collect();
    let multi = lines.len() > 1;
    // Blank lines inside a multi-line selection are left alone (quotes give
    // them a bare `>` so the quote stays one block).
    let skip_blank = multi && lines.iter().any(|&(_, _, blank)| !blank);
    let considered = || {
        lines
            .iter()
            .filter(|&&(_, _, blank)| !(skip_blank && blank))
    };

    let mut splices = Vec::new();
    let mut put = |range: Range<usize>, new: String| {
        if text[range.clone()] != *new {
            splices.push(Splice { range, text: new });
        }
    };
    match cmd {
        MdCommand::Heading(level) => {
            let level = level.clamp(1, 6);
            let off = considered().all(|(_, prefix, _)| prefix.heading == level);
            for (line, prefix, _) in considered() {
                let marker = if off {
                    String::new()
                } else {
                    format!("{} ", "#".repeat(usize::from(level)))
                };
                put(
                    line.start + prefix.list_end..line.start + prefix.content_start,
                    marker,
                );
            }
        }
        MdCommand::Paragraph => {
            for (line, prefix, _) in considered() {
                put(
                    line.start + prefix.list_end..line.start + prefix.content_start,
                    String::new(),
                );
            }
        }
        MdCommand::BulletList | MdCommand::TaskList | MdCommand::NumberedList => {
            let off = considered().all(|(_, prefix, _)| is_list_kind(cmd, prefix.list));
            let mut numbering = Numbering::continuing(text, lines[0].0);
            for (line, prefix, _) in considered() {
                let range = line.start + prefix.indent_end..line.start + prefix.list_end;
                let marker = match cmd {
                    _ if off => String::new(),
                    MdCommand::BulletList | MdCommand::TaskList
                        if is_list_kind(cmd, prefix.list) =>
                    {
                        continue;
                    }
                    MdCommand::BulletList => "- ".to_owned(),
                    MdCommand::TaskList => "- [ ] ".to_owned(),
                    _ => format!("{}. ", numbering.next(prefix.indent_end - prefix.quote_end)),
                };
                put(range, marker);
            }
        }
        MdCommand::Blockquote => {
            let off = considered().all(|(_, prefix, _)| prefix.quote_end > 0);
            for &(line, prefix, blank) in &lines {
                if off && prefix.quote_end > 0 {
                    // Remove one level: up to 3 spaces, `>`, one space.
                    let bytes = line.content(text).as_bytes();
                    let spaces = bytes.iter().take_while(|&&b| b == b' ').count();
                    let end = skip_one_space(bytes, spaces + 1);
                    put(line.start..line.start + end, String::new());
                } else if !off && prefix.quote_end == 0 {
                    let marker = if blank && multi { ">" } else { "> " };
                    put(line.start..line.start, marker.to_owned());
                }
            }
        }
        _ => {}
    }

    let selection = if sel.is_empty() {
        let caret = map_pos(&splices, sel.start, true);
        caret..caret
    } else {
        // A selection starting at a line start keeps covering the new prefix.
        let start_after = !is_line_start(text, sel.start);
        map_pos(&splices, sel.start, start_after)..map_pos(&splices, sel.end, true)
    };
    Change { splices, selection }
}

// ── Inline toggles ──────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Inline {
    Bold,
    Italic,
    Strike,
    Code,
}

impl Inline {
    /// Marker characters recognised when unwrapping.
    const fn chars(self) -> &'static [u8] {
        match self {
            Self::Bold | Self::Italic => b"*_",
            Self::Strike => b"~",
            Self::Code => b"`",
        }
    }

    /// Number of marker chars to remove from each side when runs of `left`
    /// and `right` marker chars surround the content; `None` when the
    /// content is not formatted this way.
    fn unwrap_len(self, left: usize, right: usize) -> Option<usize> {
        let n = left.min(right);
        match self {
            Self::Bold => (n >= 2).then_some(2),
            Self::Italic => (!n.is_multiple_of(2)).then_some(1),
            Self::Strike => (n >= 1).then_some(n.min(2)),
            Self::Code => (n >= 1).then_some(n),
        }
    }

    /// `false` when a run of `len` marker chars inside the content would
    /// close the span early (so unwrapping would break the markup).
    const fn run_allowed_inside(self, len: usize, unwrap_len: usize) -> bool {
        match self {
            Self::Bold => len < 2,
            Self::Italic => len.is_multiple_of(2),
            Self::Strike => len < unwrap_len,
            Self::Code => len != unwrap_len,
        }
    }

    fn wrap_marker(self, content: &str) -> String {
        match self {
            Self::Bold => "**".to_owned(),
            Self::Italic => "*".to_owned(),
            Self::Strike => "~~".to_owned(),
            Self::Code => "`".repeat(longest_run(content.as_bytes(), b'`') + 1),
        }
    }
}

fn longest_run(bytes: &[u8], c: u8) -> usize {
    bytes.split(|&b| b != c).map(<[u8]>::len).max().unwrap_or(0)
}

const fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b >= 0x80
}

/// If the content of `seg` is formatted as `kind`, return the content range
/// and the number of marker chars to remove on each side.  Marker chars at
/// the edges of `seg` count as part of the markers; runs are only looked
/// for within `bounds` (the line content).
fn find_wrapped(
    text: &str,
    seg: &Range<usize>,
    bounds: &Range<usize>,
    kind: Inline,
) -> Option<(usize, usize, usize)> {
    let bytes = text.as_bytes();
    kind.chars().iter().find_map(|&c| {
        let (mut cs, mut ce) = (seg.start, seg.end);
        while cs < ce && bytes[cs] == c {
            cs += 1;
        }
        while ce > cs && bytes[ce - 1] == c {
            ce -= 1;
        }
        if cs == ce {
            // Only marker chars (or a caret): look on both sides of the middle.
            cs = seg.start + (seg.end - seg.start) / 2;
            ce = cs;
        }
        let left = bytes[bounds.start..cs]
            .iter()
            .rev()
            .take_while(|&&b| b == c)
            .count();
        let right = bytes[ce..bounds.end]
            .iter()
            .take_while(|&&b| b == c)
            .count();
        let k = kind.unwrap_len(left, right)?;
        let inside_ok = bytes[cs..ce]
            .split(|&b| b != c)
            .all(|run| run.is_empty() || kind.run_allowed_inside(run.len(), k));
        // `_` only delimits emphasis at word boundaries.
        let flanking_ok = c != b'_'
            || ((cs - left == bounds.start || !is_word_byte(bytes[cs - left - 1]))
                && (ce + right == bounds.end || !is_word_byte(bytes[ce + right])));
        (inside_ok && flanking_ok).then_some((cs, ce, k))
    })
}

/// `range` without leading and trailing whitespace, or `None` if empty.
fn trim_range(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    let s = &text[range.clone()];
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let start = range.start + (s.len() - s.trim_start().len());
    Some(start..start + trimmed.len())
}

/// Content start (after the block prefix) and end of `line`.
fn content_bounds(text: &str, line: Line) -> Range<usize> {
    line.start + parse_prefix(line.content(text)).content_start..line.end
}

/// The selected part of each touched line's content, trimmed: `(span,
/// line content bounds)`.  Line prefixes (`- `, `## `, `> `) are skipped.
fn inline_segments(text: &str, sel: &Range<usize>) -> Vec<(Range<usize>, Range<usize>)> {
    touched_lines(text, sel)
        .filter_map(|line| {
            let bounds = content_bounds(text, line);
            let start = sel.start.max(bounds.start);
            let end = sel.end.min(bounds.end);
            if start >= end {
                return None;
            }
            trim_range(text, start..end).map(|span| (span, bounds))
        })
        .collect()
}

/// A caret moved out of the line prefix (so `- ` stays a list marker).
fn caret_in_content(text: &str, pos: usize) -> (usize, Range<usize>) {
    let bounds = content_bounds(text, line_at(text, pos));
    (pos.max(bounds.start), bounds)
}

fn inline_toggle(text: &str, sel: &Range<usize>, kind: Inline) -> Change {
    let segments = if sel.is_empty() {
        Vec::new()
    } else {
        inline_segments(text, sel)
    };
    if segments.is_empty() {
        let (caret, bounds) = caret_in_content(text, sel.start);
        if let Some((cs, _, k)) = find_wrapped(text, &(caret..caret), &bounds, kind) {
            // Caret inside an empty pair: remove it.
            let splices = vec![
                Splice {
                    range: cs - k..cs,
                    text: String::new(),
                },
                Splice {
                    range: cs..cs + k,
                    text: String::new(),
                },
            ];
            return Change {
                splices,
                selection: cs - k..cs - k,
            };
        }
        let marker = kind.wrap_marker("");
        let inside = caret + marker.len();
        return Change {
            splices: vec![Splice {
                range: caret..caret,
                text: marker.repeat(2),
            }],
            selection: inside..inside,
        };
    }

    let wrapped: Vec<_> = segments
        .iter()
        .map(|(span, bounds)| find_wrapped(text, span, bounds, kind))
        .collect();
    let mut splices = Vec::new();
    let single = segments.len() == 1;
    // With one segment the new selection is its content, without markers.
    let mut content = 0..0;
    let selection = if wrapped.iter().all(Option::is_some) {
        for (cs, ce, k) in wrapped.into_iter().flatten() {
            splices.push(Splice {
                range: cs - k..cs,
                text: String::new(),
            });
            splices.push(Splice {
                range: ce..ce + k,
                text: String::new(),
            });
            content = cs - k..ce - k;
        }
        if single {
            content
        } else {
            map_pos(&splices, sel.start, true)..map_pos(&splices, sel.end, true)
        }
    } else {
        for ((span, _), found) in segments.iter().zip(&wrapped) {
            if found.is_some() {
                continue;
            }
            let marker = kind.wrap_marker(&text[span.clone()]);
            content = span.start + marker.len()..span.end + marker.len();
            splices.push(Splice {
                range: span.start..span.start,
                text: marker.clone(),
            });
            splices.push(Splice {
                range: span.end..span.end,
                text: marker,
            });
        }
        if single {
            content
        } else {
            map_pos(&splices, sel.start, true)..map_pos(&splices, sel.end, false)
        }
    };
    Change { splices, selection }
}

// ── Link ────────────────────────────────────────────────────────────────

fn looks_like_url(s: &str) -> bool {
    const SCHEMES: [&str; 5] = ["http://", "https://", "ftp://", "mailto:", "www."];
    !s.contains(char::is_whitespace)
        && (s.contains("://")
            || SCHEMES.iter().any(|scheme| {
                s.get(..scheme.len())
                    .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
            }))
}

fn link(text: &str, sel: &Range<usize>) -> Change {
    let (start, _) = caret_in_content(text, sel.start);
    let span = trim_range(text, start..sel.end.max(start));
    let (range, insert, selection) = match span {
        None => {
            let insert = "[text](url)".to_owned();
            (start..start, insert, start + 1..start + 1 + "text".len())
        }
        Some(span) => {
            let selected = &text[span.clone()];
            if looks_like_url(selected) {
                let insert = format!("[text]({selected})");
                let at = span.start + 1;
                (span, insert, at..at + "text".len())
            } else {
                let insert = format!("[{selected}](url)");
                let at = span.start + 1 + selected.len() + "](".len();
                (span, insert, at..at + "url".len())
            }
        }
    };
    Change {
        splices: vec![Splice {
            range,
            text: insert,
        }],
        selection,
    }
}

// ── Blocks ──────────────────────────────────────────────────────────────

/// Fence char and length when `line` opens a fenced code block.
fn fence(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let c = *trimmed.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let len = trimmed.bytes().take_while(|&b| b == c).count();
    (len >= 3).then_some((c, len))
}

/// `true` when `open` opens a fence that `close` closes.
fn fences_match(open: &str, close: &str) -> bool {
    match (fence(open), fence(close)) {
        (Some((c1, n1)), Some((c2, n2))) => c1 == c2 && n2 >= n1 && close.trim().len() == n2,
        _ => false,
    }
}

/// Insert `block` (lines joined with `eol`) on its own
/// lines after the last touched line, replacing that line when it is blank,
/// with blank lines around it so it does not merge into a neighbouring
/// paragraph.  Returns the splice and where the block starts in the new
/// text.
fn insert_block(text: &str, sel: &Range<usize>, block: &[&str], eol: &str) -> (Splice, usize) {
    let line = touched_lines(text, sel)
        .last()
        .unwrap_or_else(|| line_at(text, sel.start));
    let (range, blank_lines_before) = if line.is_blank(text) {
        let prev_blank = line_before(text, line).is_none_or(|prev| prev.is_blank(text));
        (line.start..line.end, usize::from(!prev_blank))
    } else {
        (line.end..line.end, 2)
    };
    // A line after the block is needed for the caret, and a blank one before
    // any following paragraph.
    let blank_after = match line_after(text, line) {
        None => 1,
        Some(next) => usize::from(!next.is_blank(text)),
    };
    let pre = eol.repeat(blank_lines_before);
    let block_start = range.start + pre.len();
    let insert = format!("{pre}{}{}", block.join(eol), eol.repeat(blank_after));
    (
        Splice {
            range,
            text: insert,
        },
        block_start,
    )
}

fn code_block(text: &str, sel: &Range<usize>, eol: &str) -> Change {
    let lines: Vec<Line> = touched_lines(text, sel).collect();
    let (first, last) = (lines[0], lines[lines.len() - 1]);

    // The selection includes both fences: remove them.
    if lines.len() >= 2 && fences_match(first.content(text), last.content(text)) {
        let inner = if lines.len() == 2 {
            ""
        } else {
            &text[first.next..lines[lines.len() - 2].end]
        };
        return Change {
            splices: vec![Splice {
                range: first.start..last.end,
                text: inner.to_owned(),
            }],
            selection: first.start..first.start + inner.len(),
        };
    }

    // Fences right around the touched lines: remove them.
    if let (Some(open), Some(close)) = (line_before(text, first), line_after(text, last))
        && fences_match(open.content(text), close.content(text))
    {
        let inner = &text[first.start..last.end];
        let shift = first.start - open.start;
        let selection = if sel.is_empty() {
            sel.start - shift..sel.start - shift
        } else {
            open.start..open.start + inner.len()
        };
        return Change {
            splices: vec![Splice {
                range: open.start..close.end,
                text: inner.to_owned(),
            }],
            selection,
        };
    }

    if sel.is_empty() {
        let (splice, block_start) = insert_block(text, sel, &["```", "", "```"], eol);
        let caret = block_start + "```".len() + eol.len();
        return Change {
            splices: vec![splice],
            selection: caret..caret,
        };
    }

    let inner = &text[first.start..last.end];
    let longest_inner_fence = inner
        .lines()
        .filter_map(fence)
        .filter(|&(c, _)| c == b'`')
        .map(|(_, len)| len)
        .max()
        .unwrap_or(2);
    let marker = "`".repeat(longest_inner_fence.max(2) + 1);
    let start = first.start + marker.len() + eol.len();
    Change {
        splices: vec![Splice {
            range: first.start..last.end,
            text: format!("{marker}{eol}{inner}{eol}{marker}"),
        }],
        selection: start..start + inner.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::MdCommand::{
        Blockquote, Bold, BulletList, CodeBlock, Heading, HorizontalRule, InlineCode, Italic, Link,
        NumberedList, Paragraph, Strikethrough, Table, TaskList,
    };
    use super::*;

    const CARET: char = '‸';
    const SEL_START: char = '«';
    const SEL_END: char = '»';

    /// `"a«b»c"` → `("abc", 1..2)`; `"a‸bc"` → `("abc", 1..1)`.
    fn parse(marked: &str) -> (String, Range<usize>) {
        let mut text = String::new();
        let (mut start, mut end) = (None, None);
        for c in marked.chars() {
            let at = text.chars().count();
            match c {
                CARET => {
                    start = Some(at);
                    end = Some(at);
                }
                SEL_START => start = Some(at),
                SEL_END => end = Some(at),
                _ => text.push(c),
            }
        }
        let start = start.unwrap_or_else(|| unreachable!("no selection in {marked:?}"));
        (text, start..end.unwrap_or(start))
    }

    fn render(text: &str, sel: &Range<usize>) -> String {
        let mut out = String::new();
        for (i, c) in text.chars().enumerate() {
            push_markers(&mut out, i, sel);
            out.push(c);
        }
        push_markers(&mut out, text.chars().count(), sel);
        out
    }

    fn push_markers(out: &mut String, i: usize, sel: &Range<usize>) {
        if sel.is_empty() {
            if i == sel.start {
                out.push(CARET);
            }
        } else if i == sel.start {
            out.push(SEL_START);
        } else if i == sel.end {
            out.push(SEL_END);
        }
    }

    /// [`super::apply`] in the text's own line-ending style (LF without
    /// line breaks).
    fn apply(cmd: MdCommand, text: &str, selection: Range<usize>) -> Edit {
        let eol = EndOfLine::detect(text).unwrap_or(EndOfLine::Lf);
        super::apply(cmd, text, selection, eol)
    }

    /// Run `cmd` on a marked-up text and return the marked-up result.
    fn run(cmd: MdCommand, marked: &str) -> String {
        let text = parse(marked).0;
        run_in(
            cmd,
            marked,
            EndOfLine::detect(&text).unwrap_or(EndOfLine::Lf),
        )
    }

    /// [`run`] in a document whose line-ending style is `eol`.
    fn run_in(cmd: MdCommand, marked: &str, eol: EndOfLine) -> String {
        let (text, sel) = parse(marked);
        let edit = super::apply(cmd, &text, sel, eol);
        let mut out = text;
        edit.apply_to(&mut out);
        render(&out, &edit.selection)
    }

    #[track_caller]
    fn check(cmd: MdCommand, before: &str, after: &str) {
        assert_eq!(run(cmd, before), after, "{cmd:?} on {before:?}");
    }

    // ── Inline toggles ──

    #[test]
    fn bold_wraps_selection_and_unwraps_it_again() {
        check(Bold, "a «word» b", "a **«word»** b");
        check(Bold, "a **«word»** b", "a «word» b");
        check(Bold, "a «**word**» b", "a «word» b");
    }

    #[test]
    fn inline_markers_skip_selected_whitespace() {
        check(Bold, "a« word »b", "a **«word»** b");
        check(Italic, "«  x\t»", "  *«x»*\t");
    }

    #[test]
    fn empty_selection_inserts_pair_and_second_press_removes_it() {
        check(Bold, "a ‸b", "a **‸**b");
        check(Bold, "a **‸**b", "a ‸b");
        check(Italic, "‸", "*‸*");
        check(Strikethrough, "x‸", "x~~‸~~");
        check(InlineCode, "x‸y", "x`‸`y");
        check(InlineCode, "x`‸`y", "x‸y");
    }

    #[test]
    fn italic_and_bold_combine() {
        check(Italic, "«**b**»", "*«**b**»*");
        check(Bold, "«*i*»", "**«*i*»**");
        check(Italic, "***«x»***", "**«x»**");
        check(Bold, "***«x»***", "*«x»*");
        check(Italic, "*«x»*", "«x»");
        check(Bold, "__«x»__", "«x»");
        check(Italic, "_«x»_", "«x»");
    }

    #[test]
    fn underscore_inside_words_is_not_emphasis() {
        check(Italic, "snake_«case»_name", "snake_*«case»*_name");
    }

    #[test]
    fn unwrap_is_refused_when_it_would_break_inner_markup() {
        check(Bold, "«**a** and **b**»", "**«**a** and **b**»**");
    }

    #[test]
    fn strikethrough_and_code_toggle() {
        check(Strikethrough, "«gone»", "~~«gone»~~");
        check(Strikethrough, "~~«gone»~~", "«gone»");
        check(Strikethrough, "~«gone»~", "«gone»");
        check(InlineCode, "«x»", "`«x»`");
        check(InlineCode, "«`x`»", "«x»");
        check(InlineCode, "«a`b»", "``«a`b»``");
    }

    #[test]
    fn inline_toggle_skips_line_prefixes() {
        check(Bold, "«## Title»", "## **«Title»**");
        check(Bold, "«- item»", "- **«item»**");
        check(Bold, "> «quote»", "> **«quote»**");
        check(Bold, "‸- item", "- **‸**item");
    }

    #[test]
    fn inline_toggle_wraps_each_line_of_a_multi_line_selection() {
        check(Bold, "«- a\n- b»", "«- **a**\n- **b»**");
        check(Bold, "«- **a**\n- **b**»", "«- a\n- b»");
        check(Bold, "«a\n\nb»\n", "**«a**\n\n**b»**\n");
        // Only the lines not yet bold are wrapped.
        check(Bold, "«**a**\nb»", "«**a**\n**b»**");
    }

    #[test]
    fn inline_toggle_with_multibyte_text() {
        check(Bold, "日本«語»🦀", "日本**«語»**🦀");
        check(Bold, "日本**«語»**🦀", "日本«語»🦀");
        check(Italic, "🦀‸🦀", "🦀*‸*🦀");
        check(InlineCode, "«ü» é", "`«ü»` é");
    }

    // ── Link ──

    #[test]
    fn url_detection_handles_multibyte_text() {
        assert!(looks_like_url("https://example.com/日本"));
        assert!(looks_like_url("WWW.example.com"));
        assert!(!looks_like_url("日本"));
        assert!(!looks_like_url("ww"));
        assert!(!looks_like_url("http:// spaced"));
    }

    #[test]
    fn link_uses_selection_as_text_or_url() {
        check(Link, "see ‸", "see [«text»](url)");
        check(Link, "see «docs»", "see [docs](«url»)");
        check(
            Link,
            "see «https://example.com»",
            "see [«text»](https://example.com)",
        );
        check(Link, "«www.rust-lang.org»", "[«text»](www.rust-lang.org)");
        check(Link, "«not a url»", "[not a url](«url»)");
        check(Link, "«- item»", "- [item](«url»)");
        check(Link, "«日本» x", "[日本](«url») x");
    }

    // ── Headings ──

    #[test]
    fn heading_toggles_replaces_and_removes() {
        check(Heading(2), "Ti‸tle", "## Ti‸tle");
        check(Heading(2), "## Ti‸tle", "Ti‸tle");
        check(Heading(1), "## Ti‸tle", "# Ti‸tle");
        check(Heading(3), "#‸# Title", "### ‸Title");
        check(Heading(2), "‸", "## ‸");
        check(Heading(9), "x‸", "###### x‸");
        check(Heading(0), "x‸", "# x‸");
        check(Paragraph, "### Ti‸tle", "Ti‸tle");
        check(Paragraph, "Ti‸tle", "Ti‸tle");
    }

    #[test]
    fn heading_applies_to_every_touched_line_and_skips_blank_ones() {
        check(Heading(1), "«a\n\nb»", "«# a\n\n# b»");
        check(Heading(1), "«# a\n\n# b»", "«a\n\nb»");
        check(Heading(1), "«# a\nb»", "«# a\n# b»");
        check(Paragraph, "«# a\n## b»", "«a\nb»");
    }

    #[test]
    fn heading_goes_inside_list_items_and_quotes() {
        check(Heading(2), "- it‸em", "- ## it‸em");
        check(Heading(2), "> quo‸te", "> ## quo‸te");
    }

    #[test]
    fn heading_level_reports_shared_level() {
        let level = |marked: &str| {
            let (text, sel) = parse(marked);
            heading_level(&text, sel)
        };
        assert_eq!(level("## a‸"), Some(2));
        assert_eq!(level("a‸"), Some(0));
        assert_eq!(level("«# a\n\n# b»"), Some(1));
        assert_eq!(level("«# a\n## b»"), None);
        assert_eq!(level("‸"), Some(0));
        assert_eq!(level("# a\r\n‸## b\r\n"), Some(2));
    }

    // ── Lists and quotes ──

    #[test]
    fn bullet_list_toggles_and_preserves_indentation() {
        check(BulletList, "it‸em", "- it‸em");
        check(BulletList, "- it‸em", "it‸em");
        check(BulletList, "«a\nb»", "«- a\n- b»");
        check(BulletList, "«- a\n- b»", "«a\nb»");
        check(BulletList, "«  a\n    b»", "«  - a\n    - b»");
        check(BulletList, "«* a\nb»", "«* a\n- b»");
        check(BulletList, "‸", "- ‸");
    }

    #[test]
    fn numbered_list_numbers_consecutively() {
        check(NumberedList, "«a\nb\nc»", "«1. a\n2. b\n3. c»");
        check(NumberedList, "«1. a\n2. b»", "«a\nb»");
        check(NumberedList, "«1. a\nb\n7. c»", "«1. a\n2. b\n3. c»");
        check(
            NumberedList,
            "«a\n  b\n  c\nd»",
            "«1. a\n  1. b\n  2. c\n2. d»",
        );
        check(NumberedList, "«a\n\nb»", "«1. a\n\n2. b»");
    }

    #[test]
    fn numbered_list_continues_the_list_above() {
        check(NumberedList, "1. a\n2. b\n‸c", "1. a\n2. b\n3. ‸c");
    }

    #[test]
    fn task_list_toggles() {
        check(TaskList, "to‸do", "- [ ] to‸do");
        check(TaskList, "- [x] do‸ne", "do‸ne");
        check(TaskList, "«- [ ] a\nb»", "«- [ ] a\n- [ ] b»");
    }

    #[test]
    fn switching_list_kind_replaces_the_marker() {
        check(NumberedList, "- it‸em", "1. it‸em");
        check(BulletList, "3. it‸em", "- it‸em");
        check(TaskList, "- it‸em", "- [ ] it‸em");
        check(BulletList, "- [ ] it‸em", "- it‸em");
        check(TaskList, "1. it‸em", "- [ ] it‸em");
    }

    #[test]
    fn caret_inside_a_removed_marker_moves_to_the_content() {
        check(BulletList, "-‸ item", "‸item");
        check(NumberedList, "1‸. item", "‸item");
    }

    #[test]
    fn blockquote_toggles_and_keeps_blank_lines_inside() {
        check(Blockquote, "quo‸te", "> quo‸te");
        check(Blockquote, "> quo‸te", "quo‸te");
        check(Blockquote, ">quo‸te", "quo‸te");
        check(Blockquote, "«a\n\nb»", "«> a\n>\n> b»");
        check(Blockquote, "«> a\n>\n> b»", "«a\n\nb»");
        check(Blockquote, "«- a\n- b»", "«> - a\n> - b»");
        check(Blockquote, "«> a\nb»", "«> a\n> b»");
    }

    #[test]
    fn selection_ending_after_a_line_break_does_not_touch_the_next_line() {
        check(BulletList, "«a\n»b", "«- a\n»b");
        check(Heading(1), "«a\r\n»b", "«# a\r\n»b");
        check(Bold, "«a\n»b", "**«a»**\nb");
    }

    #[test]
    fn line_prefixes_with_crlf() {
        check(BulletList, "a\r\n«b\r\nc»\r\nd", "a\r\n«- b\r\n- c»\r\nd");
        check(NumberedList, "«a\r\nb»\r\n", "«1. a\r\n2. b»\r\n");
        check(Blockquote, "«a\r\n\r\nb»", "«> a\r\n>\r\n> b»");
        check(Heading(2), "a\r\n‸b\r\n", "a\r\n## ‸b\r\n");
    }

    #[test]
    fn selection_boundary_inside_crlf_is_moved_before_cr() {
        // The caret sits between `\r` and `\n`.
        let (text, _) = parse("ab‸\r\nc");
        let edit = apply(Bold, &text, 3..3);
        let mut out = text;
        edit.apply_to(&mut out);
        assert_eq!(out, "ab****\r\nc");
        assert_eq!(edit.selection, 4..4);
    }

    #[test]
    fn line_prefixes_with_multibyte_text() {
        check(BulletList, "«日本\n🦀🦀»", "«- 日本\n- 🦀🦀»");
        check(Heading(1), "🦀‸🦀", "# 🦀‸🦀");
        check(NumberedList, "é\n‸ü", "é\n1. ‸ü");
    }

    // ── Blocks ──

    #[test]
    fn code_block_wraps_touched_lines_and_unwraps() {
        check(CodeBlock, "«a\nb»", "```\n«a\nb»\n```");
        check(CodeBlock, "```\n«a\nb»\n```", "«a\nb»");
        check(CodeBlock, "«```\na\n```»", "«a»");
        check(CodeBlock, "x\n«a»\ny", "x\n```\n«a»\n```\ny");
        check(CodeBlock, "p«ar»tial", "```\n«partial»\n```");
    }

    #[test]
    fn code_block_fence_is_longer_than_inner_fences() {
        check(CodeBlock, "«```\na»", "````\n«```\na»\n````");
    }

    #[test]
    fn empty_code_block_has_caret_inside() {
        check(CodeBlock, "‸", "```\n‸\n```\n");
        check(CodeBlock, "text‸", "text\n\n```\n‸\n```\n");
        check(CodeBlock, "a\n‸\nb", "a\n\n```\n‸\n```\n\nb");
        check(CodeBlock, "```\n‸\n```", "‸");
    }

    #[test]
    fn code_block_with_crlf() {
        check(CodeBlock, "«a\r\nb»\r\n", "```\r\n«a\r\nb»\r\n```\r\n");
        check(CodeBlock, "x‸\r\n", "x\r\n\r\n```\r\n‸\r\n```\r\n");
    }

    #[test]
    fn horizontal_rule_gets_blank_lines_around_it() {
        check(HorizontalRule, "‸", "---\n‸");
        check(HorizontalRule, "text‸", "text\n\n---\n‸");
        check(HorizontalRule, "te‸xt\nmore", "text\n\n---\n‸\nmore");
        check(HorizontalRule, "a\n‸\nb", "a\n\n---\n‸\nb");
        check(HorizontalRule, "a\n\n‸\n\nb", "a\n\n---\n‸\nb");
        check(HorizontalRule, "a\n\n‸", "a\n\n---\n‸");
        check(HorizontalRule, "«a\nb»\nc", "a\nb\n\n---\n‸\nc");
    }

    #[test]
    fn inserted_line_breaks_use_the_document_style_not_the_text() {
        use EndOfLine::{CrLf, Lf};
        // No line break yet (a new Untitled document on Windows).
        assert_eq!(
            run_in(CodeBlock, "x‸", CrLf),
            "x\r\n\r\n```\r\n‸\r\n```\r\n"
        );
        assert_eq!(run_in(HorizontalRule, "x‸", CrLf), "x\r\n\r\n---\r\n‸");
        assert_eq!(run_in(HorizontalRule, "x‸", Lf), "x\n\n---\n‸");
        // A mixed document whose first line break is not its style.
        assert_eq!(
            run_in(CodeBlock, "«a»\nb\r\nc\r\n", CrLf),
            "```\r\n«a»\r\n```\nb\r\nc\r\n"
        );
    }

    #[test]
    fn horizontal_rule_with_crlf_never_splits_a_pair() {
        check(
            HorizontalRule,
            "text‸\r\nmore",
            "text\r\n\r\n---\r\n‸\r\nmore",
        );
        // The caret sits between `\r` and `\n`.
        let edit = apply(HorizontalRule, "ab\r\nc", 3..3);
        let mut out = "ab\r\nc".to_owned();
        edit.apply_to(&mut out);
        assert_eq!(out, "ab\r\n\r\n---\r\n\r\nc");
    }

    #[test]
    fn table_template_selects_first_header_cell() {
        check(
            Table,
            "‸",
            "| «Column 1» | Column 2 |\n| -------- | -------- |\n| Text     | Text     |\n| Text     | Text     |\n",
        );
        check(
            Table,
            "intro‸\r\nnext",
            "intro\r\n\r\n| «Column 1» | Column 2 |\r\n| -------- | -------- |\r\n| Text     | Text     |\r\n| Text     | Text     |\r\n\r\nnext",
        );
    }

    #[test]
    fn block_insertion_after_multibyte_text() {
        check(HorizontalRule, "日本‸", "日本\n\n---\n‸");
        check(
            Table,
            "🦀‸",
            "🦀\n\n| «Column 1» | Column 2 |\n| -------- | -------- |\n| Text     | Text     |\n| Text     | Text     |\n",
        );
    }

    // ── Edits ──

    #[test]
    fn edits_are_minimal() {
        let edit = apply(Bold, "abc word def", 4..8);
        assert_eq!(edit.replace, 4..8);
        assert_eq!(edit.insert, "**word**");
        let edit = apply(BulletList, "one\ntwo\n", 5..5);
        assert_eq!((edit.replace, edit.insert.as_str()), (4..4, "- "));
        // Unwrapping removes only the markers.
        let edit = apply(Bold, "x **y** z", 4..5);
        assert_eq!(edit.replace, 2..7);
        assert_eq!(edit.insert, "y");
        // Multi-byte text before the edit is counted in chars.
        let edit = apply(BulletList, "🦀\nb", 2..2);
        assert_eq!(edit.replace, 2..2);
        assert_eq!(edit.selection, 4..4);
    }

    #[test]
    fn noop_keeps_text_and_selection() {
        let edit = apply(Paragraph, "plain", 1..3);
        assert!(edit.is_noop());
        assert_eq!(edit.selection, 1..3);
    }

    #[test]
    fn reversed_and_out_of_range_selections_are_clamped() {
        assert_eq!(run(Bold, "«ab»"), "**«ab»**");
        let edit = apply(Bold, "ab", Range { start: 2, end: 0 });
        assert_eq!(edit.insert, "**ab**");
        let edit = apply(Bold, "ab", 50..60);
        assert_eq!((edit.replace, edit.insert.as_str()), (2..2, "****"));
    }

    #[test]
    fn every_command_keeps_selection_on_char_boundaries() {
        let commands = [
            Bold,
            Italic,
            Strikethrough,
            InlineCode,
            Link,
            Heading(1),
            Heading(6),
            Paragraph,
            BulletList,
            NumberedList,
            TaskList,
            Blockquote,
            CodeBlock,
            HorizontalRule,
            Table,
        ];
        let text = "# 日本\r\n\r\n- 🦀 **b** `c`\r\n> q\r\n1. x\r\n```\r\ncode\r\n```\r\n";
        let len = text.chars().count();
        for cmd in commands {
            for start in 0..=len {
                for end in [start, (start + 3).min(len), len] {
                    let edit = apply(cmd, text, start..end);
                    let mut out = text.to_owned();
                    edit.apply_to(&mut out);
                    let out_len = out.chars().count();
                    assert!(
                        edit.selection.start <= edit.selection.end && edit.selection.end <= out_len,
                        "{cmd:?} {start}..{end}: {:?} in {out:?}",
                        edit.selection
                    );
                    assert!(
                        !out.contains("\r\r")
                            && out.matches('\r').count() == out.matches("\r\n").count(),
                        "{cmd:?} {start}..{end} split a CRLF: {out:?}"
                    );
                    assert!(
                        !out.replace("\r\n", "").contains('\n'),
                        "{cmd:?} {start}..{end} inserted a bare LF: {out:?}"
                    );
                }
            }
        }
    }
}
