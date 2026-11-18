#![forbid(unsafe_code)]
//! Formatted view of the editor (Windows 11 Notepad / Obsidian "live
//! preview" style): headings big, bold bold, links in link colour, and the
//! Markdown syntax marks (`##`, `**`, `[`…`](url)`, `> `, `- `, fences, …)
//! hidden on every line except the ones holding the caret or selection,
//! where they show (weak) so they can be edited.
//!
//! The document text never changes.  A hidden mark is laid out with a
//! near-zero font size ([`HIDDEN_FONT_SIZE`]) in a transparent colour, so the
//! galley still has one glyph per char and every char index ↔ caret
//! position mapping (selection, Ln/Col, find, go to line, scroll sync)
//! stays exact.  What a glyph cannot show — bullets, checkboxes, quote bars,
//! rules, code-block backgrounds and table grids — is painted from
//! [`Overlay`]s placed on the galley after layout.
//!
//! Tables line up as a grid: their cells are moved into columns with the
//! sections' leading space (measured, so the text still never changes); off
//! the caret the pipes and the `|---|` row are hidden and grid lines drawn,
//! with the caret in the table its source shows, still lined up.
//!
//! Work is split by what it depends on:
//! - [`analyze`]: the text only (cached per edit),
//! - [`layout_job`]: + the caret's lines and the style (cheap),
//! - [`place_overlays`]: + the galley.

use std::ops::Range;

use eframe::egui::{
    self, Color32, FontFamily, FontId, Painter, Pos2, Rect, Stroke, TextFormat, layers::ShapeIdx,
    text::LayoutJob,
};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Font size of hidden marks.  egui does not clamp font sizes (tested down
/// to 1e-6): a glyph still gets laid out, so the char ↔ caret mapping
/// holds, and at this size even 1000 marks in a row advance less than a
/// pixel (at 0.01 a long link URL would still shift the line by ~2 px).
pub const HIDDEN_FONT_SIZE: f32 = 1e-4;

/// Documents larger than this show as raw text in Edit.  The formatted
/// view costs much more than the raw view per keystroke (parse + many
/// sections) and lays the document out again when the caret moves to a
/// line with marks.
pub const MAX_FORMATTED_BYTES: usize = 512 * 1024;
/// [`MAX_FORMATTED_BYTES`] for people.
pub const MAX_FORMATTED_LABEL: &str = "512 KB";

/// Inline code and code blocks, relative to the text around them (as the
/// preview; see `ui_style::MONOSPACE_SCALE`).
const CODE_FONT_SCALE: f32 = crate::ui_style::MONOSPACE_SCALE;
/// Leading list indentation: monospace at this fraction of the body size,
/// so two spaces of nesting are about the preview's 16 px list indent.
const INDENT_FONT_SCALE: f32 = 0.65;
/// Task brackets (`[ ]`): monospace at this fraction of the body size, a
/// little wider than the checkbox painted over them.
const TASK_FONT_SCALE: f32 = 0.5;
/// Horizontal room per quote level (bar + gaps), in body sizes.
const QUOTE_INDENT_EM: f32 = 1.0;
/// Quote bar: left margin in body sizes, and width (as the preview).
const QUOTE_BAR_MARGIN_EM: f32 = 0.3;
const QUOTE_BAR_WIDTH: f32 = 3.0;
/// Left padding of code-block lines (the preview's code-block margin).
pub const CODE_PAD: f32 = 6.0;
/// Space above a heading, as a fraction of its size (as the preview).
const HEADING_TOP_SPACING_EM: f32 = 0.3;
/// Space between a table cell's text and the grid line on either side, in
/// body sizes.
const TABLE_PAD_EM: f32 = 0.5;

/// Per-byte style flags (see [`Analysis`]).
mod flag {
    /// Heading level 1–6 (0 = not a heading).
    pub const HEADING: u16 = 0b111;
    pub const STRONG: u16 = 1 << 3;
    pub const EMPH: u16 = 1 << 4;
    pub const STRIKE: u16 = 1 << 5;
    /// Inline code span.
    pub const CODE: u16 = 1 << 6;
    pub const LINK: u16 = 1 << 7;
    /// Image alt text.
    pub const IMAGE: u16 = 1 << 8;
    pub const CODE_BLOCK: u16 = 1 << 9;
    pub const TABLE: u16 = 1 << 10;
    /// Syntax: hidden off the caret's lines.
    pub const MARK: u16 = 1 << 11;
    /// With `MARK`: invisible but keeps its width; an overlay is painted
    /// there (list bullet, task checkbox).
    pub const BOX: u16 = 1 << 12;
    /// Info string (language) of a code fence.
    pub const FENCE_INFO: u16 = 1 << 13;
    /// Leading indentation of a list line.
    pub const INDENT: u16 = 1 << 14;
    /// With `BOX`: the brackets of a task checkbox.
    pub const TASK: u16 = 1 << 15;
}

/// Row height of a line whose every char is a hidden mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spacer {
    /// The line has visible chars.
    None,
    /// Code fences and setext underlines: a thin row.
    Small,
    /// Rules and empty quote lines: a body-text row.
    Body,
}

#[derive(Clone, Copy, Debug)]
struct Line {
    /// Byte and char offset of the line start.
    start: u32,
    start_char: u32,
    /// Index of the line's first run in [`Analysis::runs`].
    first_run: u32,
    /// Lines before this one whose layout changes when revealed (prefix
    /// count, see [`Analysis::reveal_for`]).
    marked_before: u32,
    quote_depth: u8,
    /// Chars at the line start that are marks (a click there lands after
    /// them).
    prefix_chars: u32,
    spacer: Spacer,
    /// Inside a code block (fences included): padded on the left.
    code: bool,
}

/// What [`place_overlays`] paints for a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decoration {
    /// `- ` / `* ` / `+ `, painted as a bullet; `depth` is the list nesting.
    Bullet { depth: u8 },
    /// `[ ]` / `[x]`; `toggle_byte` is the byte between the brackets.
    Checkbox { checked: bool, toggle_byte: u32 },
    /// Thematic break (`---`).
    Rule,
    /// Code block background down to (and including) `last_line`.
    CodeBlock { last_line: u32 },
}

/// A table: its lines (indices) and its columns' alignment.
#[derive(Clone, Debug)]
struct Table {
    first_line: u32,
    last_line: u32,
    /// The `|---|---|` line.
    delimiter_line: u32,
    align: Vec<Alignment>,
}

/// A Mermaid code block: shown as its diagram's picture off the caret
/// (see [`hide_diagrams`]).
#[derive(Clone, Debug)]
pub struct DiagramBlock {
    /// The diagram's code (between the fences).
    pub code: Range<usize>,
    /// Lines of the whole block, fences included, and of its code.
    first_line: u32,
    last_line: u32,
    first_code_line: u32,
    last_code_line: u32,
}

/// What a piece of a table line is (see [`table_line_parts`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    /// Before the first cell: quote marks, indentation, the leading pipe.
    Prefix,
    /// Cell `n`, with the spaces around its text.
    Cell(usize),
    /// The pipe after cell `n`.
    Pipe(usize),
    /// After the last cell: spaces after the closing pipe, the line break.
    Rest,
}

#[derive(Clone, Copy, Debug)]
struct Placed {
    line: u32,
    char_index: u32,
    decoration: Decoration,
}

/// The formatting of a text: style runs and decorations.  Depends on the
/// text only, so the editor keeps it until the next edit.
#[derive(Debug, Default)]
pub struct Analysis {
    len: usize,
    /// `(start byte, flags)`; a run ends where the next one starts (the
    /// last at `len`).  Runs never cross a line start.
    runs: Vec<(u32, u16)>,
    lines: Vec<Line>,
    /// Lines whose layout changes when revealed.
    marked_lines: u32,
    /// Sorted by char index.
    decorations: Vec<Placed>,
    /// In document order.
    tables: Vec<Table>,
    /// Mermaid code blocks, in document order.
    diagrams: Vec<DiagramBlock>,
}

/// Lines `first..=last` (indices) shown with their marks: the lines
/// holding the caret or the selection.
pub type Reveal = Option<(u32, u32)>;

/// Parse `text` and record how each byte is styled and which bytes are
/// syntax marks.
#[must_use]
pub fn analyze(text: &str) -> Analysis {
    let len = text.len();
    if u32::try_from(len).is_err() {
        // Far beyond `MAX_FORMATTED_BYTES`; callers use the raw view.
        return Analysis::default();
    }
    let bytes = text.as_bytes();
    let mut flags = vec![0u16; len];
    let mut found = Found::default();
    scan_events(text, &mut flags, &mut found);

    let line_starts = line_starts(text);
    let line_of = |byte: usize| {
        line_starts
            .partition_point(|&s| s <= byte)
            .saturating_sub(1)
    };

    // Per-line quote depth and list membership from the block ranges
    // (difference arrays over line indices).
    let mut quote_delta = vec![0i32; line_starts.len() + 1];
    for range in &found.quotes {
        quote_delta[line_of(range.start)] += 1;
        quote_delta[line_of(range.end.saturating_sub(1).max(range.start)) + 1] -= 1;
    }
    let mut list_delta = vec![0i32; line_starts.len() + 1];
    for range in &found.lists {
        list_delta[line_of(range.start)] += 1;
        list_delta[line_of(range.end.saturating_sub(1).max(range.start)) + 1] -= 1;
    }
    let mut code_line = vec![false; line_starts.len()];
    for (range, _) in &found.code_blocks {
        let last = line_of(range.end.saturating_sub(1).max(range.start));
        code_line[line_of(range.start)..=last].fill(true);
    }
    // A Mermaid block's lines change layout when revealed (picture ↔ code).
    let mut diagram_line = vec![false; line_starts.len()];
    for (block, _) in &found.diagrams {
        let last = line_of(block.end.saturating_sub(1).max(block.start));
        diagram_line[line_of(block.start)..=last].fill(true);
    }
    let mut small_line = vec![false; line_starts.len()];
    for &byte in &found.small_lines {
        small_line[line_of(byte)] = true;
    }

    let mut analysis = Analysis {
        len,
        runs: Vec::with_capacity(len / 24 + line_starts.len()),
        lines: Vec::with_capacity(line_starts.len()),
        marked_lines: 0,
        decorations: Vec::new(),
        tables: Vec::with_capacity(found.tables.len()),
        diagrams: Vec::with_capacity(found.diagrams.len()),
    };
    let (mut quote_depth, mut in_list, mut start_char) = (0i32, 0i32, 0usize);
    let mut marked_before = 0u32;
    for (index, &start) in line_starts.iter().enumerate() {
        quote_depth += quote_delta[index];
        in_list += list_delta[index];
        let end = line_starts.get(index + 1).copied().unwrap_or(len);
        let content_end = content_end(bytes, start, end);
        let depth = quote_depth.clamp(0, 8) as usize;
        let mut pos = hide_quote_prefix(bytes, &mut flags, start, content_end, depth);
        if in_list > 0 {
            while pos < content_end
                && matches!(bytes[pos], b' ' | b'\t')
                && flags[pos] & flag::CODE_BLOCK == 0
            {
                flags[pos] = flag::INDENT;
                pos += 1;
            }
        }
        // Line breaks: `\n` is body text (it sizes a following empty
        // line); `\r` is styled like the char before it.
        for byte in content_end..end {
            flags[byte] = if bytes[byte] == b'\r' && byte > start {
                flags[byte - 1]
            } else {
                0
            };
        }

        let line_flags = &flags[start..content_end];
        let all_hidden = !line_flags.is_empty()
            && line_flags
                .iter()
                .all(|&f| f & flag::MARK != 0 && f & flag::BOX == 0);
        let spacer = if !all_hidden {
            Spacer::None
        } else if small_line[index] {
            Spacer::Small
        } else {
            Spacer::Body
        };
        let prefix_bytes = line_flags
            .iter()
            .take_while(|&&f| f & flag::MARK != 0)
            .count();
        let line_text = &text[start..end];
        analysis.lines.push(Line {
            start: start as u32,
            start_char: start_char as u32,
            first_run: analysis.runs.len() as u32,
            marked_before,
            quote_depth: depth as u8,
            prefix_chars: text[start..start + prefix_bytes].chars().count() as u32,
            spacer,
            code: code_line[index],
        });
        start_char += line_text.chars().count();
        if depth > 0 || diagram_line[index] || line_flags.iter().any(|&f| f & flag::MARK != 0) {
            marked_before += 1;
        }
        push_runs(&mut analysis.runs, &flags, start, end);
    }
    analysis.marked_lines = marked_before;
    // After a final line break the caret can sit on one more (empty) line.
    if len == 0 || bytes[len - 1] == b'\n' {
        analysis.lines.push(Line {
            start: len as u32,
            start_char: start_char as u32,
            first_run: analysis.runs.len() as u32,
            marked_before,
            quote_depth: 0,
            prefix_chars: 0,
            spacer: Spacer::None,
            code: false,
        });
    }

    let mut decorations = Vec::with_capacity(found.decorations.len() + found.code_blocks.len());
    for &(byte, decoration) in &found.decorations {
        let line = line_of(byte);
        decorations.push(analysis.place(text, line, byte, decoration));
    }
    for (range, _) in &found.code_blocks {
        let line = line_of(range.start);
        let last_line = line_of(range.end.saturating_sub(1).max(range.start)) as u32;
        let byte = line_starts[line];
        decorations.push(analysis.place(text, line, byte, Decoration::CodeBlock { last_line }));
    }
    decorations.sort_by_key(|placed| placed.char_index);
    analysis.decorations = decorations;
    for (block, code) in found.diagrams {
        let line = |byte: usize| line_of(byte) as u32;
        analysis.diagrams.push(DiagramBlock {
            first_line: line(block.start),
            last_line: line(block.end.saturating_sub(1).max(block.start)),
            first_code_line: line(code.start),
            last_code_line: line(code.end.saturating_sub(1).max(code.start)),
            code,
        });
    }
    for (range, align) in found.tables {
        let first_line = line_of(range.start) as u32;
        let last_line = line_of(range.end.saturating_sub(1).max(range.start)) as u32;
        analysis.tables.push(Table {
            first_line,
            last_line,
            delimiter_line: (first_line + 1).min(last_line),
            align,
        });
    }
    analysis
}

/// Block ranges and decorations collected from the parser events.
#[derive(Default)]
struct Found {
    quotes: Vec<Range<usize>>,
    lists: Vec<Range<usize>>,
    /// Fenced (`true`) or indented code blocks.
    code_blocks: Vec<(Range<usize>, bool)>,
    /// `(byte, decoration)` for bullets, checkboxes and rules.
    decorations: Vec<(usize, Decoration)>,
    /// A byte on each line that gets a thin row when hidden.
    small_lines: Vec<usize>,
    /// Each table's range and column alignments.
    tables: Vec<(Range<usize>, Vec<Alignment>)>,
    /// Each Mermaid block's range and its code's range.
    diagrams: Vec<(Range<usize>, Range<usize>)>,
    /// The Mermaid block being read: its range and its code so far.
    open_diagram: Option<(Range<usize>, Option<Range<usize>>)>,
}

/// Walk the `pulldown-cmark` events: an inline container (emphasis, link,
/// heading, …) marks its whole range as syntax, then its content events
/// overwrite their own ranges as content, which leaves exactly the
/// delimiters marked.
fn scan_events(text: &str, flags: &mut [u16], found: &mut Found) {
    let bytes = text.as_bytes();
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM;
    let mut current: u16 = 0;
    let mut stack: Vec<u16> = Vec::new();
    // Per open list item: index of its bullet in `found.decorations`.
    let mut items: Vec<Option<usize>> = Vec::new();
    let mut list_depth = 0usize;

    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        let range = range.start.min(flags.len())..range.end.min(flags.len());
        match event {
            Event::Start(tag) => {
                stack.push(current);
                match tag {
                    Tag::Heading { level, .. } => {
                        current = (current & !flag::HEADING) | level as u16;
                        flags[range].fill(current | flag::MARK);
                    }
                    Tag::Emphasis => mark_container(flags, range, &mut current, flag::EMPH),
                    Tag::Strong => mark_container(flags, range, &mut current, flag::STRONG),
                    Tag::Strikethrough => {
                        mark_container(flags, range, &mut current, flag::STRIKE);
                    }
                    Tag::Table(align) => {
                        // Pipes, the `|---|` row and the spaces around cell
                        // text stay marks; the cells' content overwrites
                        // its own ranges.
                        current |= flag::TABLE;
                        flags[range.clone()].fill(current | flag::MARK);
                        if let Some(end) = memchr::memchr(b'\n', &bytes[range.clone()]) {
                            found.small_lines.push(range.start + end + 1);
                        }
                        found.tables.push((range, align));
                    }
                    // Header cells in bold, as the preview.
                    Tag::TableHead => current |= flag::STRONG,
                    Tag::Link { .. } => mark_container(flags, range, &mut current, flag::LINK),
                    Tag::Image { .. } => mark_container(flags, range, &mut current, flag::IMAGE),
                    Tag::CodeBlock(CodeBlockKind::Fenced(info)) => {
                        let language = info.split_whitespace().next().unwrap_or_default();
                        if language.eq_ignore_ascii_case("mermaid") {
                            found.open_diagram = Some((range.clone(), None));
                        }
                        current |= flag::CODE_BLOCK;
                        flags[range.clone()].fill(current | flag::MARK);
                        fence_lines(bytes, flags, &range, current, &mut found.small_lines);
                        found.code_blocks.push((range, true));
                    }
                    Tag::CodeBlock(CodeBlockKind::Indented) => {
                        current |= flag::CODE_BLOCK;
                        flags[range.clone()].fill(current);
                        found.code_blocks.push((range, false));
                    }
                    Tag::BlockQuote(_) => found.quotes.push(range),
                    Tag::List(_) => {
                        list_depth += 1;
                        found.lists.push(range);
                    }
                    Tag::Item => {
                        let bullet = hide_bullet(bytes, flags, range.start);
                        items.push(bullet.map(|byte| {
                            let depth = list_depth.saturating_sub(1).min(u8::MAX.into()) as u8;
                            found.decorations.push((byte, Decoration::Bullet { depth }));
                            found.decorations.len() - 1
                        }));
                    }
                    _ => {}
                }
            }
            Event::End(tag) => {
                match tag {
                    TagEnd::List(_) => list_depth = list_depth.saturating_sub(1),
                    TagEnd::Item => {
                        items.pop();
                    }
                    TagEnd::Heading(_) => setext_underline(bytes, &range, &mut found.small_lines),
                    TagEnd::CodeBlock => {
                        if let Some((block, Some(code))) = found.open_diagram.take()
                            && code.start < code.end
                        {
                            found.diagrams.push((block, code));
                        }
                    }
                    // An image without alt text shows its file name (dim),
                    // so it is visible off the caret line (pasted images).
                    TagEnd::Image
                        if bytes[range.clone()].starts_with(b"![](") && range.len() > 5 =>
                    {
                        flags[range.start + 4..range.end - 1].fill(current);
                    }
                    _ => {}
                }
                current = stack.pop().unwrap_or(0);
            }
            Event::Code(_) => code_span(bytes, flags, range, current),
            Event::TaskListMarker(checked) => {
                if range.end > range.start + 1 {
                    // The brackets and the space after them.
                    let end = if bytes.get(range.end) == Some(&b' ') {
                        range.end + 1
                    } else {
                        range.end
                    };
                    flags[range.start..end].fill(flag::MARK | flag::BOX | flag::TASK);
                    let checkbox = Decoration::Checkbox {
                        checked,
                        toggle_byte: (range.start + 1) as u32,
                    };
                    // The checkbox replaces the item's bullet.
                    if let Some(Some(index)) = items.last()
                        && let Some((byte, decoration)) = found.decorations.get_mut(*index)
                    {
                        let bullet_end = bullet_end(bytes, *byte);
                        flags[*byte..bullet_end].fill(flag::MARK);
                        *byte = range.start;
                        *decoration = checkbox;
                    } else {
                        found.decorations.push((range.start, checkbox));
                    }
                }
            }
            Event::Rule => {
                flags[range.clone()].fill(flag::MARK);
                found.decorations.push((range.start, Decoration::Rule));
            }
            Event::Text(_) if found.open_diagram.is_some() => {
                if let Some((_, code)) = &mut found.open_diagram {
                    *code = Some(code.as_ref().map_or_else(
                        || range.clone(),
                        |code| code.start.min(range.start)..code.end.max(range.end),
                    ));
                }
                flags[range].fill(current);
            }
            Event::Text(_)
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::SoftBreak
            | Event::HardBreak => flags[range].fill(current),
        }
    }
}

/// Start an inline container with style `add`: its whole range is marked
/// until its content events overwrite their parts.
fn mark_container(flags: &mut [u16], range: Range<usize>, current: &mut u16, add: u16) {
    *current |= add;
    flags[range].fill(*current | flag::MARK);
}

/// An inline code span: the backtick runs are marks.
fn code_span(bytes: &[u8], flags: &mut [u16], range: Range<usize>, current: u16) {
    let style = current | flag::CODE;
    flags[range.clone()].fill(style);
    let ticks = bytes[range.clone()]
        .iter()
        .take_while(|&&b| b == b'`')
        .count();
    let closing = bytes[range.clone()]
        .iter()
        .rev()
        .take_while(|&&b| b == b'`')
        .count()
        .min(range.len().saturating_sub(ticks));
    flags[range.start..range.start + ticks].fill(style | flag::MARK);
    flags[range.end - closing..range.end].fill(style | flag::MARK);
}

/// Opening and closing lines of a fenced code block: the info string
/// (language) stays visible; both lines get thin rows when hidden.
fn fence_lines(
    bytes: &[u8],
    flags: &mut [u16],
    range: &Range<usize>,
    current: u16,
    small_lines: &mut Vec<usize>,
) {
    let line_end = memchr::memchr(b'\n', &bytes[range.start..range.end])
        .map_or(range.end, |pos| range.start + pos);
    let mut pos = range.start;
    while pos < line_end && bytes[pos] == b' ' {
        pos += 1;
    }
    let fence = bytes.get(pos).copied().unwrap_or(b'`');
    while pos < line_end && bytes[pos] == fence {
        pos += 1;
    }
    let info = trim_range(bytes, pos..line_end);
    flags[info].fill(current | flag::FENCE_INFO);
    small_lines.push(range.start);

    // The closing fence is the last line of the range, when there is one.
    let body = trim_line_break(bytes, range.start..range.end);
    if let Some(last_start) =
        memchr::memrchr(b'\n', &bytes[body.clone()]).map(|p| body.start + p + 1)
    {
        let last = trim_range(bytes, last_start..body.end);
        if !last.is_empty() && bytes[last].iter().all(|&b| b == fence) {
            small_lines.push(last_start);
        }
    }
}

/// A setext heading's underline (`===` / `---`) gets a thin row.
fn setext_underline(bytes: &[u8], range: &Range<usize>, small_lines: &mut Vec<usize>) {
    let body = trim_line_break(bytes, range.clone());
    let Some(last_start) = memchr::memrchr(b'\n', &bytes[body.clone()]).map(|p| body.start + p + 1)
    else {
        return;
    };
    let last = trim_range(bytes, last_start..body.end);
    if !last.is_empty() && bytes[last].iter().all(|&b| matches!(b, b'=' | b'-')) {
        small_lines.push(last_start);
    }
}

/// Hide a bullet list marker (`- `, `* `, `+ `) that starts at or after
/// `start`; returns its byte.  Ordered markers (`1.`) stay visible.
fn hide_bullet(bytes: &[u8], flags: &mut [u16], start: usize) -> Option<usize> {
    let mut pos = start;
    while pos < bytes.len() && bytes[pos] == b' ' {
        pos += 1;
    }
    if !matches!(bytes.get(pos), Some(b'-' | b'*' | b'+')) {
        return None;
    }
    let end = bullet_end(bytes, pos);
    flags[pos..end].fill(flag::MARK | flag::BOX);
    Some(pos)
}

/// End of the bullet at `pos` and the spaces after it (up to 4).
fn bullet_end(bytes: &[u8], pos: usize) -> usize {
    let mut end = pos + 1;
    while end < bytes.len() && end - pos <= 4 && matches!(bytes[end], b' ' | b'\t') {
        end += 1;
    }
    end
}

/// Mark the `depth` quote markers (`> `, after up to 3 spaces) at the
/// start of a line; returns the byte after them.
fn hide_quote_prefix(
    bytes: &[u8],
    flags: &mut [u16],
    start: usize,
    end: usize,
    depth: usize,
) -> usize {
    let mut pos = start;
    for _ in 0..depth {
        let mut probe = pos;
        while probe < end && probe - pos < 3 && bytes[probe] == b' ' {
            probe += 1;
        }
        if probe >= end || bytes[probe] != b'>' {
            break;
        }
        probe += 1;
        if probe < end && matches!(bytes[probe], b' ' | b'\t') {
            probe += 1;
        }
        flags[pos..probe].fill(flag::MARK);
        pos = probe;
    }
    pos
}

/// Byte offsets of the line starts (a line includes its line break).
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(text.len() / 40 + 1);
    if !text.is_empty() {
        starts.push(0);
    }
    starts.extend(
        memchr::memchr_iter(b'\n', text.as_bytes())
            .map(|pos| pos + 1)
            .filter(|&start| start < text.len()),
    );
    starts
}

/// End of a line's content (before its `\r\n` / `\n`).
fn content_end(bytes: &[u8], start: usize, end: usize) -> usize {
    let mut content = end;
    while content > start && matches!(bytes[content - 1], b'\n' | b'\r') {
        content -= 1;
    }
    content
}

fn trim_line_break(bytes: &[u8], range: Range<usize>) -> Range<usize> {
    range.start..content_end(bytes, range.start, range.end)
}

fn trim_range(bytes: &[u8], range: Range<usize>) -> Range<usize> {
    let (mut start, mut end) = (range.start, range.end);
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    start..end
}

/// Split `start..end` (one line) into runs of equal flags.
fn push_runs(runs: &mut Vec<(u32, u16)>, flags: &[u16], start: usize, end: usize) {
    let mut run_flags = None;
    for (byte, &f) in flags.iter().enumerate().take(end).skip(start) {
        if run_flags != Some(f) {
            runs.push((byte as u32, f));
            run_flags = Some(f);
        }
    }
}

impl Analysis {
    fn place(&self, text: &str, line: usize, byte: usize, decoration: Decoration) -> Placed {
        let info = self.lines[line];
        let start = info.start as usize;
        let chars = text[start..byte.max(start)].chars().count();
        Placed {
            line: line as u32,
            char_index: info.start_char + chars as u32,
            decoration,
        }
    }

    /// Byte length of the analysed text.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Index of the line holding char `char_index`.
    fn line_of_char(&self, char_index: usize) -> usize {
        self.lines
            .partition_point(|line| line.start_char as usize <= char_index)
            .saturating_sub(1)
    }

    /// The lines to reveal for a selection between char indices `a` and
    /// `b` (in either order): the lines it touches (a table it touches
    /// whole), or `None` when none of them has marks, so moving the caret
    /// between plain lines keeps the layout.
    #[must_use]
    pub fn reveal_for(&self, a: usize, b: usize) -> Reveal {
        if self.lines.is_empty() {
            return None;
        }
        let mut first = self.line_of_char(a.min(b));
        let mut last = self.line_of_char(a.max(b));
        let blocks = self
            .tables
            .iter()
            .map(|table| (table.first_line, table.last_line))
            .chain(
                self.diagrams
                    .iter()
                    .map(|diagram| (diagram.first_line, diagram.last_line)),
            );
        for (top, bottom) in blocks {
            let (top, bottom) = (top as usize, bottom as usize);
            if top <= last && first <= bottom {
                first = first.min(top);
                last = last.max(bottom);
            }
        }
        let marked_until = |line: usize| {
            self.lines
                .get(line)
                .map_or(self.marked_lines, |line| line.marked_before)
        };
        let marked = marked_until(last + 1) - marked_until(first);
        (marked > 0).then_some((first as u32, last as u32))
    }

    /// When char `char_index` lies in the marks at the start of its line
    /// (`## `, `> `, `- `, …), the char index just after them.
    #[must_use]
    pub fn after_line_prefix(&self, char_index: usize) -> Option<usize> {
        let line = self.lines.get(self.line_of_char(char_index))?;
        let end = (line.start_char + line.prefix_chars) as usize;
        (char_index < end).then_some(end)
    }

    /// The Mermaid code blocks.
    #[must_use]
    pub fn diagrams(&self) -> &[DiagramBlock] {
        &self.diagrams
    }

    /// Index of the line holding byte `byte`.
    fn line_of_byte(&self, byte: usize) -> usize {
        self.lines
            .partition_point(|line| line.start as usize <= byte)
            .saturating_sub(1)
    }

    /// `(byte range, flags)` of every run of line `index`.
    fn line_runs(&self, index: usize) -> impl Iterator<Item = (Range<usize>, u16)> + '_ {
        let first = self.lines[index].first_run as usize;
        let last = self
            .lines
            .get(index + 1)
            .map_or(self.runs.len(), |line| line.first_run as usize);
        (first..last).map(move |run| {
            let (start, f) = self.runs[run];
            let end = self
                .runs
                .get(run + 1)
                .map_or(self.len, |next| next.0 as usize);
            (start as usize..end, f)
        })
    }
}

const fn is_revealed(reveal: Reveal, line: usize) -> bool {
    match reveal {
        Some((first, last)) => first as usize <= line && line <= last as usize,
        None => false,
    }
}

/// Fonts, colours and sizes of the formatted view, matching the preview
/// (`md/render`): body text at the Body text style's size, headings at
/// that times [`crate::md::HEADING_FONT_SCALES`], in bold.
#[derive(Clone, Debug)]
pub struct FormatStyle {
    body_size: f32,
    /// Bold font family, when the fonts have one (`ui_style::bold_family`).
    bold: Option<FontFamily>,
    /// The headings' family when the theme has its own (Paper's serif).
    heading: Option<FontFamily>,
    small_size: f32,
    text: Color32,
    weak: Color32,
    link: Color32,
    code_bg: Color32,
    rule: Color32,
    /// Table grid lines.
    grid: Color32,
    /// Per heading level: colour and row height (with top spacing).
    headings: [(Color32, f32); 6],
    body_row: f32,
    small_row: f32,
}

impl FormatStyle {
    /// `row_height` measures a font's row height (`FontsView::row_height`).
    pub fn new(
        style: &egui::Style,
        visuals: &egui::Visuals,
        heading_color_mode: bool,
        bold: Option<FontFamily>,
        heading: Option<FontFamily>,
        mut row_height: impl FnMut(&FontId) -> f32,
    ) -> Self {
        let body_size = egui::TextStyle::Body.resolve(style).size;
        let small_size = egui::TextStyle::Small.resolve(style).size;
        let heading_family = heading
            .clone()
            .or_else(|| bold.clone())
            .unwrap_or(FontFamily::Proportional);
        let headings = std::array::from_fn(|index| {
            let size = body_size * crate::md::HEADING_FONT_SCALES[index];
            let row = row_height(&FontId::new(size, heading_family.clone()));
            (
                crate::md::heading_color(visuals, index + 1, heading_color_mode),
                HEADING_TOP_SPACING_EM.mul_add(size, row).round(),
            )
        });
        let body_row = row_height(&FontId::proportional(body_size));
        Self {
            body_size,
            bold,
            heading,
            small_size,
            text: visuals.text_color(),
            weak: visuals.weak_text_color(),
            link: visuals.hyperlink_color,
            code_bg: visuals.faint_bg_color,
            rule: visuals.weak_text_color(),
            grid: visuals.widgets.noninteractive.bg_stroke.color,
            headings,
            body_row,
            small_row: (body_row * 0.5).round(),
        }
    }

    fn quote_indent(&self) -> f32 {
        (self.body_size * QUOTE_INDENT_EM).round()
    }

    /// Font of a list bullet or of task brackets (`flags` has `BOX`).
    fn box_font(&self, flags: u16) -> FontId {
        if flags & flag::TASK == 0 {
            FontId::monospace(self.body_size)
        } else {
            FontId::monospace(self.body_size * TASK_FONT_SCALE)
        }
    }

    /// The format of a run with `flags` on a hidden (`revealed == false`)
    /// or revealed line.
    fn format(&self, flags: u16, revealed: bool, spacer: Spacer) -> TextFormat {
        let mark = flags & flag::MARK != 0;
        if mark && !revealed {
            if flags & flag::BOX != 0 {
                return TextFormat::simple(self.box_font(flags), Color32::TRANSPARENT);
            }
            let mut hidden =
                TextFormat::simple(FontId::proportional(HIDDEN_FONT_SIZE), Color32::TRANSPARENT);
            hidden.line_height = match spacer {
                Spacer::None => None,
                Spacer::Small => Some(self.small_row),
                Spacer::Body => Some(self.body_row),
            };
            return hidden;
        }

        let level = usize::from(flags & flag::HEADING);
        let (size, mut color, line_height) = match level {
            1..=6 => {
                let (color, row) = self.headings[level - 1];
                let size = self.body_size * crate::md::HEADING_FONT_SCALES[level - 1];
                (size, color, Some(row))
            }
            _ => (self.body_size, self.text, None),
        };
        let mut format = TextFormat {
            line_height,
            ..TextFormat::default()
        };
        format.font_id = if flags & flag::FENCE_INFO != 0 {
            color = self.weak;
            FontId::proportional(self.small_size)
        } else if flags & (flag::CODE | flag::CODE_BLOCK) != 0 {
            FontId::monospace(size * CODE_FONT_SCALE)
        } else if flags & flag::BOX != 0 {
            self.box_font(flags)
        } else if flags & flag::INDENT != 0 {
            FontId::monospace(self.body_size * INDENT_FONT_SCALE)
        } else if level != 0
            && let Some(heading) = &self.heading
        {
            // Headings in the theme's heading font (Paper's serif).
            FontId::new(size, heading.clone())
        } else if (flags & flag::STRONG != 0 || level != 0)
            && let Some(bold) = &self.bold
        {
            // Strong text, and headings (all bold).
            FontId::new(size, bold.clone())
        } else {
            FontId::proportional(size)
        };
        if flags & flag::LINK != 0 {
            color = self.link;
        } else if flags & flag::IMAGE != 0 {
            color = self.weak;
        } else if flags & flag::STRONG != 0 {
            color = crate::md::strengthen_color(color);
        }
        if mark {
            color = self.weak;
        } else if flags & flag::LINK != 0 {
            format.underline = Stroke::new(1.0, color);
        }
        if flags & flag::CODE != 0 {
            format.background = self.code_bg;
        }
        format.italics = flags & flag::EMPH != 0;
        if flags & flag::STRIKE != 0 && !mark {
            format.strikethrough = Stroke::new(1.0, color);
        }
        format.color = color;
        format
    }
}

/// The layout job of the formatted view: marks hidden except on the
/// `reveal` lines.  `measure` gives the width of a text in a font (tables
/// are lined up with it).
#[must_use]
pub fn layout_job(
    text: &str,
    analysis: &Analysis,
    reveal: Reveal,
    style: &FormatStyle,
    mut measure: impl FnMut(&str, &FontId) -> f32,
) -> LayoutJob {
    let mut job = LayoutJob {
        text: text.to_owned(),
        sections: Vec::with_capacity(analysis.runs.len()),
        ..LayoutJob::default()
    };
    if analysis.len != text.len() {
        // Stale analysis: plain text rather than wrong styling.
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: 0..text.len(),
            format: TextFormat::simple(FontId::proportional(style.body_size), style.text),
        });
        return job;
    }
    let mut tables = analysis.tables.iter().peekable();
    let mut index = 0;
    while index < analysis.lines.len() {
        while tables
            .next_if(|table| (table.first_line as usize) < index)
            .is_some()
        {}
        if let Some(table) = tables.next_if(|table| table.first_line as usize == index) {
            push_table(&mut job, text, analysis, table, reveal, style, &mut measure);
            index = table.last_line as usize + 1;
            continue;
        }
        push_line(&mut job, analysis, index, reveal, style);
        index += 1;
    }
    job
}

/// Leading space of a line: its quote indent (not on a revealed line,
/// whose `>` marks show where the bars were) and code-block padding.
fn line_leading(line: &Line, style: &FormatStyle, revealed: bool) -> f32 {
    let quote = if revealed { 0 } else { line.quote_depth };
    let mut leading = f32::from(quote) * style.quote_indent();
    if line.code {
        leading += CODE_PAD;
    }
    leading
}

/// The sections of line `index`.
fn push_line(
    job: &mut LayoutJob,
    analysis: &Analysis,
    index: usize,
    reveal: Reveal,
    style: &FormatStyle,
) {
    let line = &analysis.lines[index];
    let revealed = is_revealed(reveal, index);
    let mut leading = line_leading(line, style, revealed);
    for (byte_range, flags) in analysis.line_runs(index) {
        let format = style.format(flags, revealed, line.spacer);
        let leading_space = std::mem::take(&mut leading);
        if leading_space == 0.0
            && let Some(last) = job.sections.last_mut()
            && last.byte_range.end == byte_range.start
            && last.format == format
        {
            last.byte_range.end = byte_range.end;
            continue;
        }
        job.sections.push(egui::text::LayoutSection {
            leading_space,
            byte_range,
            format,
        });
    }
}

/// The parts of table line `index` (contiguous, in order), from its pipes:
/// a `|` that is a mark (not `\|`, not in code).  Without a leading pipe
/// the first cell starts where the table does on the line.
fn table_line_parts(text: &str, analysis: &Analysis, index: usize) -> Vec<(Range<usize>, Part)> {
    let bytes = text.as_bytes();
    let line = &analysis.lines[index];
    let start = line.start as usize;
    let end = analysis
        .lines
        .get(index + 1)
        .map_or(analysis.len, |next| next.start as usize);
    let content_end = content_end(bytes, start, end);
    let runs: Vec<(Range<usize>, u16)> = analysis.line_runs(index).collect();
    let is_mark = |byte: usize| {
        runs.iter()
            .find(|(range, _)| range.contains(&byte))
            .is_some_and(|&(_, f)| f & flag::MARK != 0)
    };
    let pipe_flags = flag::TABLE | flag::MARK;
    let pipes: Vec<usize> = runs
        .iter()
        .filter(|&&(_, f)| (f & pipe_flags) == pipe_flags)
        .flat_map(|(range, _)| range.clone())
        .filter(|&byte| byte < content_end && bytes[byte] == b'|')
        .collect();
    let table_start = runs
        .iter()
        .find(|(_, f)| f & flag::TABLE != 0)
        .map_or(content_end, |(range, _)| range.start);

    let mut parts = Vec::with_capacity(pipes.len() * 2 + 3);
    let mut pipes = pipes.into_iter().peekable();
    // Only quote marks and spaces before it (the `|---|` row is all marks).
    let leading_pipe = pipes.peek().is_some_and(|&first| {
        (start..first).all(|byte| is_mark(byte) && matches!(bytes[byte], b' ' | b'\t' | b'>'))
    });
    let mut cell_start = match pipes.peek().copied() {
        Some(first) if leading_pipe => {
            pipes.next();
            first + 1
        }
        _ => table_start,
    };
    parts.push((start..cell_start, Part::Prefix));
    let mut cell = 0;
    for pipe in pipes {
        parts.push((cell_start..pipe, Part::Cell(cell)));
        parts.push((pipe..pipe + 1, Part::Pipe(cell)));
        cell += 1;
        cell_start = pipe + 1;
    }
    // Text after the last pipe is a last cell without a closing pipe.
    if bytes[cell_start.min(content_end)..content_end]
        .iter()
        .any(|byte| !byte.is_ascii_whitespace())
    {
        parts.push((cell_start..content_end, Part::Cell(cell)));
        cell_start = content_end;
    }
    parts.push((cell_start..end, Part::Rest));
    parts.retain(|(range, _)| !range.is_empty());
    parts
}

/// One piece of a table line: a run (or part of one) inside one part.
struct Piece {
    range: Range<usize>,
    part: Part,
    format: TextFormat,
    width: f32,
}

/// The sections of `table`, its cells lined up in columns.
fn push_table(
    job: &mut LayoutJob,
    text: &str,
    analysis: &Analysis,
    table: &Table,
    reveal: Reveal,
    style: &FormatStyle,
    measure: &mut impl FnMut(&str, &FontId) -> f32,
) {
    let pad = (style.body_size * TABLE_PAD_EM).round();

    // Every line's pieces, measured.
    let mut rows: Vec<(f32, Vec<Piece>)> = Vec::new();
    for index in table.first_line as usize..=table.last_line as usize {
        let Some(line) = analysis.lines.get(index) else {
            break;
        };
        let revealed = is_revealed(reveal, index);
        let parts = table_line_parts(text, analysis, index);
        let mut pieces = Vec::new();
        for (run, flags) in analysis.line_runs(index) {
            for (part_range, part) in &parts {
                let range = run.start.max(part_range.start)..run.end.min(part_range.end);
                if range.is_empty() {
                    continue;
                }
                let format = style.format(flags, revealed, line.spacer);
                let width = if format.font_id.size <= HIDDEN_FONT_SIZE || *part == Part::Rest {
                    0.0
                } else {
                    measure(&text[range.clone()], &format.font_id)
                };
                pieces.push(Piece {
                    range,
                    part: *part,
                    format,
                    width,
                });
            }
        }
        rows.push((line_leading(line, style, revealed), pieces));
    }

    // Column widths: the widest cell (and pipe) of each column.
    let mut prefix_end = 0.0f32;
    let mut cell_widths: Vec<f32> = Vec::new();
    let mut pipe_widths: Vec<f32> = Vec::new();
    let grow = |widths: &mut Vec<f32>, column: usize, width: f32| {
        if widths.len() <= column {
            widths.resize(column + 1, 0.0);
        }
        widths[column] = widths[column].max(width);
    };
    for (leading, pieces) in &rows {
        let width_of = |wanted: Part| {
            pieces
                .iter()
                .filter(|piece| piece.part == wanted)
                .map(|piece| piece.width)
                .sum::<f32>()
        };
        prefix_end = prefix_end.max(leading + width_of(Part::Prefix));
        for piece in pieces {
            match piece.part {
                Part::Cell(column) => grow(&mut cell_widths, column, width_of(piece.part)),
                Part::Pipe(column) => grow(&mut pipe_widths, column, width_of(piece.part)),
                Part::Prefix | Part::Rest => {}
            }
        }
    }
    pipe_widths.resize(cell_widths.len().max(pipe_widths.len()), 0.0);
    cell_widths.resize(pipe_widths.len(), 0.0);
    let mut cell_x = Vec::with_capacity(cell_widths.len());
    let mut pipe_x = Vec::with_capacity(cell_widths.len());
    let mut x = prefix_end + pad;
    for (cell_width, pipe_width) in cell_widths.iter().zip(&pipe_widths) {
        cell_x.push(x);
        let pipe = x + cell_width + pad;
        pipe_x.push(pipe);
        x = pipe + pipe_width + pad;
    }

    for (leading, pieces) in rows {
        let cell_width = |column: usize| {
            pieces
                .iter()
                .filter(|piece| piece.part == Part::Cell(column))
                .map(|piece| piece.width)
                .sum::<f32>()
        };
        let mut x = 0.0f32;
        let mut part = None;
        let mut sections = Vec::with_capacity(pieces.len());
        for piece in &pieces {
            let leading_space = if part == Some(piece.part) {
                0.0
            } else {
                part = Some(piece.part);
                match piece.part {
                    Part::Prefix => leading,
                    Part::Cell(column) => {
                        let room = cell_widths[column] - cell_width(column);
                        let offset = match table.align.get(column) {
                            Some(Alignment::Center) => (room / 2.0).round(),
                            Some(Alignment::Right) => room,
                            _ => 0.0,
                        };
                        cell_x[column] + offset - x
                    }
                    Part::Pipe(column) => pipe_x[column] - x,
                    Part::Rest => 0.0,
                }
            };
            let leading_space = leading_space.max(0.0);
            x += leading_space + piece.width;
            sections.push(egui::text::LayoutSection {
                leading_space,
                byte_range: piece.range.clone(),
                format: piece.format.clone(),
            });
        }
        job.sections.extend(sections);
    }
}

impl DiagramBlock {
    /// Whether `reveal` shows this block's code.
    #[must_use]
    pub const fn revealed(&self, reveal: Reveal) -> bool {
        match reveal {
            Some((first, last)) => first <= self.last_line && self.first_line <= last,
            None => false,
        }
    }
}

/// Make room for diagram pictures: the code lines of each block in
/// `heights` (index into [`Analysis::diagrams`], picture height) are
/// hidden, the first one as tall as the picture, the rest of no height.
pub fn hide_diagrams(
    sections: &mut [egui::text::LayoutSection],
    analysis: &Analysis,
    heights: &[(usize, f32)],
) {
    for &(index, height) in heights {
        let Some(diagram) = analysis.diagrams.get(index) else {
            continue;
        };
        let lines = diagram.first_code_line as usize..=diagram.last_code_line as usize;
        for section in sections.iter_mut() {
            let line = analysis.line_of_byte(section.byte_range.start);
            if !lines.contains(&line) {
                continue;
            }
            let mut hidden =
                TextFormat::simple(FontId::proportional(HIDDEN_FONT_SIZE), Color32::TRANSPARENT);
            hidden.line_height = Some(if line == *lines.start() { height } else { 0.0 });
            section.format = hidden;
        }
    }
}

/// Top of the row of diagram `index`'s first code line on `galley` (where
/// its picture goes).
#[must_use]
pub fn diagram_top(galley: &egui::Galley, analysis: &Analysis, index: usize) -> Option<f32> {
    let diagram = analysis.diagrams.get(index)?;
    let line = analysis.lines.get(diagram.first_code_line as usize)?;
    let (row, _) = RowIndex::new(galley).locate(line.start_char as usize)?;
    Some(row.min_y())
}

/// Something painted over (or, for code blocks, under) the galley.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayKind {
    /// A list bullet (`•`, `◦`, `▪` by depth, as the preview).
    Bullet(&'static str),
    /// A task checkbox; clicking it toggles the char at `toggle_byte`.
    Checkbox { checked: bool, toggle_byte: usize },
    /// A horizontal rule across the editor at the rect's vertical centre.
    Rule,
    /// A blockquote bar.
    QuoteBar,
    /// A code block background across the editor.
    CodeBlock,
    /// A table's header background (painted under the text).
    TableHeader,
    /// A table's outline.
    TableBorder,
    /// A grid line inside a table, from the rect's top left to its bottom
    /// right (the rect is a line).
    TableLine,
}

/// An overlay and its rect in galley coordinates (`Rule` and `CodeBlock`
/// span the editor's width when painted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overlay {
    pub kind: OverlayKind,
    pub rect: Rect,
}

/// Galley geometry by char index.
struct RowIndex<'a> {
    galley: &'a egui::Galley,
    /// First char index of each row.
    starts: Vec<usize>,
}

impl<'a> RowIndex<'a> {
    fn new(galley: &'a egui::Galley) -> Self {
        let mut starts = Vec::with_capacity(galley.rows.len());
        let mut index = 0;
        for row in &galley.rows {
            starts.push(index);
            index += row.char_count_including_newline();
        }
        Self { galley, starts }
    }

    /// Row holding `char_index`, and the char's column in it.
    fn locate(&self, char_index: usize) -> Option<(&'a egui::epaint::text::PlacedRow, usize)> {
        let row = self
            .starts
            .partition_point(|&start| start <= char_index)
            .checked_sub(1)?;
        Some((&self.galley.rows[row], char_index - self.starts[row]))
    }

    /// The rect of chars `chars` (on one row), full row height.
    fn chars_rect(&self, chars: Range<usize>) -> Option<Rect> {
        let (row, column) = self.locate(chars.start)?;
        let x0 = row.pos.x + row.x_offset(column);
        let x1 = row.pos.x + row.x_offset(column + chars.len());
        Some(Rect::from_x_y_ranges(x0..=x1, row.min_y()..=row.max_y()))
    }

    /// Top of the row holding char `first`, bottom of the row holding char
    /// `last`.
    fn y_span(&self, first: usize, last: usize) -> Option<(f32, f32)> {
        let (top, _) = self.locate(first)?;
        let (bottom, _) = self.locate(last.max(first))?;
        Some((top.min_y(), bottom.max_y()))
    }
}

/// Place the decorations of the hidden lines (code blocks always) on
/// `galley`, which was laid out from [`layout_job`] with `reveal`.
#[must_use]
pub fn place_overlays(
    galley: &egui::Galley,
    analysis: &Analysis,
    reveal: Reveal,
    style: &FormatStyle,
) -> Vec<Overlay> {
    if analysis.len != galley.job.text.len() || galley.rows.is_empty() {
        return Vec::new();
    }
    let rows = RowIndex::new(galley);
    let last_char = |line: usize| {
        analysis
            .lines
            .get(line + 1)
            .map_or_else(|| galley.end().index, |next| next.start_char as usize)
            .saturating_sub(1)
    };
    let mut overlays = Vec::new();
    let mut decorations = analysis.decorations.iter().peekable();
    for (index, line) in analysis.lines.iter().enumerate() {
        let revealed = is_revealed(reveal, index);
        while let Some(placed) = decorations.next_if(|placed| placed.line as usize <= index) {
            let at = placed.char_index as usize;
            let overlay = match placed.decoration {
                Decoration::CodeBlock { last_line } => rows
                    .y_span(at, last_char(last_line as usize))
                    .map(|(top, bottom)| Overlay {
                        kind: OverlayKind::CodeBlock,
                        rect: Rect::from_x_y_ranges(0.0..=f32::INFINITY, top..=bottom),
                    }),
                _ if revealed => None,
                Decoration::Bullet { depth } => rows.chars_rect(at..at + 1).map(|rect| Overlay {
                    kind: OverlayKind::Bullet(bullet_glyph(depth)),
                    rect,
                }),
                Decoration::Checkbox {
                    checked,
                    toggle_byte,
                } => rows.chars_rect(at..at + 3).map(|rect| Overlay {
                    kind: OverlayKind::Checkbox {
                        checked,
                        toggle_byte: toggle_byte as usize,
                    },
                    rect,
                }),
                Decoration::Rule => rows.locate(at).map(|(row, _)| Overlay {
                    kind: OverlayKind::Rule,
                    rect: Rect::from_x_y_ranges(0.0..=f32::INFINITY, row.min_y()..=row.max_y()),
                }),
            };
            overlays.extend(overlay);
        }
        // A revealed quote line shows its `>` marks instead of bars.
        if line.quote_depth > 0
            && !revealed
            && let Some((top, bottom)) = rows.y_span(line.start_char as usize, last_char(index))
        {
            let indent = style.quote_indent();
            let margin = (style.body_size * QUOTE_BAR_MARGIN_EM).round();
            for level in 0..line.quote_depth {
                let x = f32::from(level).mul_add(indent, margin);
                overlays.push(Overlay {
                    kind: OverlayKind::QuoteBar,
                    rect: Rect::from_x_y_ranges(x..=x + QUOTE_BAR_WIDTH, top..=bottom),
                });
            }
        }
    }
    for table in &analysis.tables {
        if !is_revealed(reveal, table.first_line as usize) {
            table_grid(&mut overlays, analysis, table, &rows, &last_char, style);
        }
    }
    overlays
}

/// The grid of a table laid out by [`push_table`]: its outline, header
/// background and line, and the lines between its rows and columns, from
/// where its cells and pipes landed on the galley.
fn table_grid(
    overlays: &mut Vec<Overlay>,
    analysis: &Analysis,
    table: &Table,
    rows: &RowIndex<'_>,
    last_char: &impl Fn(usize) -> usize,
    style: &FormatStyle,
) {
    let text = rows.galley.job.text.as_str();
    let pad = (style.body_size * TABLE_PAD_EM).round();
    let x_at = |char_index: usize| {
        rows.chars_rect(char_index..char_index)
            .map(|rect| rect.left())
    };
    let (mut left, mut content_right) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut pipes: Vec<f32> = Vec::new();
    let mut columns = 0;
    let mut row_tops = Vec::new();
    let (mut top, mut bottom) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut header_bottom = None;
    for index in table.first_line as usize..=table.last_line as usize {
        let Some(line) = analysis.lines.get(index) else {
            break;
        };
        let Some((row_top, row_bottom)) = rows.y_span(line.start_char as usize, last_char(index))
        else {
            continue;
        };
        top = top.min(row_top);
        bottom = bottom.max(row_bottom);
        if index == table.delimiter_line as usize {
            header_bottom = Some(f32::midpoint(row_top, row_bottom).round());
            continue;
        }
        if index > table.delimiter_line as usize + 1 {
            row_tops.push(row_top);
        }
        let start = line.start as usize;
        let char_of = |byte: usize| line.start_char as usize + text[start..byte].chars().count();
        // Where a last cell without a closing pipe ends (the next char's
        // place would include the gap before a pipe).
        let mut open_end = None;
        for (range, part) in table_line_parts(text, analysis, index) {
            match part {
                Part::Cell(column) => {
                    columns = columns.max(column + 1);
                    if let Some(x) = x_at(char_of(range.start)) {
                        left = left.min(x);
                    }
                    open_end = x_at(char_of(range.end));
                }
                Part::Pipe(column) => {
                    open_end = None;
                    if let Some(x) = x_at(char_of(range.start)) {
                        if pipes.len() <= column {
                            pipes.resize(column + 1, f32::NEG_INFINITY);
                        }
                        pipes[column] = pipes[column].max(x);
                    }
                }
                Part::Prefix | Part::Rest => {}
            }
        }
        if let Some(x) = open_end {
            content_right = content_right.max(x);
        }
    }
    if !left.is_finite() || !top.is_finite() {
        return;
    }
    let left = (left - pad).max(0.0);
    let right = pipes.iter().copied().fold(content_right + pad, f32::max);
    if !right.is_finite() {
        return;
    }
    let crisp = |v: f32| v.round() + 0.5;
    let (left, right) = (crisp(left), crisp(right));
    let line = |a: Pos2, b: Pos2| Overlay {
        kind: OverlayKind::TableLine,
        rect: Rect::from_two_pos(a, b),
    };
    if let Some(y) = header_bottom {
        overlays.push(Overlay {
            kind: OverlayKind::TableHeader,
            rect: Rect::from_x_y_ranges(left..=right, top..=y),
        });
        let y = crisp(y);
        overlays.push(line(egui::pos2(left, y), egui::pos2(right, y)));
    }
    for y in row_tops {
        let y = crisp(y);
        overlays.push(line(egui::pos2(left, y), egui::pos2(right, y)));
    }
    for &x in pipes.iter().take(columns.saturating_sub(1)) {
        if x.is_finite() && x < right - 1.0 {
            let x = crisp(x);
            overlays.push(line(egui::pos2(x, top), egui::pos2(x, bottom)));
        }
    }
    overlays.push(Overlay {
        kind: OverlayKind::TableBorder,
        rect: Rect::from_x_y_ranges(left..=right, top..=bottom),
    });
}

const fn bullet_glyph(depth: u8) -> &'static str {
    match depth {
        0 => "\u{2022}",
        1 => "\u{25E6}",
        _ => "\u{25AA}",
    }
}

/// Paint `overlays` of a galley drawn at `origin`: code-block backgrounds
/// into the `backdrop` slot (reserved before the text was painted), the
/// rest on top.  Only overlays inside `clip` are painted; `width` is the
/// editor's width.
pub fn paint_overlays(
    painter: &Painter,
    backdrop: ShapeIdx,
    overlays: &[Overlay],
    origin: Pos2,
    width: f32,
    style: &FormatStyle,
) {
    let clip = painter.clip_rect();
    let full_width = |rect: Rect| {
        Rect::from_x_y_ranges(origin.x..=origin.x + width, rect.y_range())
            .translate(egui::vec2(0.0, origin.y))
    };
    let mut backgrounds = Vec::new();
    for overlay in overlays {
        let rect = match overlay.kind {
            OverlayKind::Rule | OverlayKind::CodeBlock => full_width(overlay.rect),
            _ => overlay.rect.translate(origin.to_vec2()),
        };
        if !rect.intersects(clip) {
            continue;
        }
        match overlay.kind {
            OverlayKind::CodeBlock => {
                backgrounds.push(egui::Shape::rect_filled(rect, 4.0, style.code_bg));
            }
            OverlayKind::TableHeader => {
                backgrounds.push(egui::Shape::rect_filled(rect, 0.0, style.code_bg));
            }
            OverlayKind::TableBorder => {
                painter.rect_stroke(
                    rect,
                    0.0,
                    Stroke::new(1.0, style.grid),
                    egui::StrokeKind::Middle,
                );
            }
            OverlayKind::TableLine => {
                painter.line_segment([rect.min, rect.max], Stroke::new(1.0, style.grid));
            }
            OverlayKind::Rule => {
                let y = rect.center().y.round() + 0.5;
                painter.line_segment(
                    [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                    Stroke::new(1.5, style.rule),
                );
            }
            OverlayKind::QuoteBar => {
                painter.rect_filled(rect, 1.0, style.weak);
            }
            OverlayKind::Bullet(glyph) => {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    glyph,
                    FontId::proportional(style.body_size),
                    style.text,
                );
            }
            OverlayKind::Checkbox { checked, .. } => paint_checkbox(painter, rect, checked, style),
        }
    }
    painter.set(backdrop, egui::Shape::Vec(backgrounds));
}

/// The square of a checkbox drawn in `rect` (the `[ ]` chars).
#[must_use]
pub fn checkbox_square(rect: Rect, style: &FormatStyle) -> Rect {
    let side = (style.body_size * 0.7).min(rect.height()).round();
    Rect::from_center_size(rect.center(), egui::vec2(side, side))
}

fn paint_checkbox(painter: &Painter, rect: Rect, checked: bool, style: &FormatStyle) {
    let square = checkbox_square(rect, style);
    if checked {
        painter.rect_filled(square, 3.0, style.link);
        let s = square.width();
        let points = [
            square.left_top() + egui::vec2(0.22 * s, 0.52 * s),
            square.left_top() + egui::vec2(0.42 * s, 0.72 * s),
            square.left_top() + egui::vec2(0.78 * s, 0.3 * s),
        ];
        painter.line(points.to_vec(), Stroke::new(1.8, Color32::WHITE));
    } else {
        painter.rect_stroke(
            square,
            3.0,
            Stroke::new(1.2, style.weak),
            egui::StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        crate::ui_style::configure_style(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx
    }

    fn style(ctx: &egui::Context) -> FormatStyle {
        let egui_style = ctx.style();
        FormatStyle::new(
            &egui_style,
            &egui_style.visuals,
            true,
            Some(crate::ui_style::bold_family()),
            None,
            |font| ctx.fonts_mut(|fonts| fonts.row_height(font)),
        )
    }

    fn job(ctx: &egui::Context, text: &str, reveal: Reveal) -> LayoutJob {
        layout_job(text, &analyze(text), reveal, &style(ctx), measure(ctx))
    }

    /// Text widths from `ctx`'s fonts, for [`layout_job`].
    fn measure(ctx: &egui::Context) -> impl FnMut(&str, &FontId) -> f32 + '_ {
        |piece: &str, font: &FontId| {
            ctx.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(piece.to_owned(), font.clone(), Color32::PLACEHOLDER)
                    .size()
                    .x
            })
        }
    }

    /// The format of the char at the first occurrence of `snippet` (plus
    /// `offset` bytes).
    fn format_at<'a>(job: &'a LayoutJob, snippet: &str, offset: usize) -> &'a TextFormat {
        let byte = job.text.find(snippet).map(|at| at + offset);
        assert!(byte.is_some(), "{snippet:?} not in {:?}", job.text);
        let byte = byte.unwrap_or_default();
        let section = job
            .sections
            .iter()
            .find(|section| section.byte_range.contains(&byte));
        assert!(section.is_some(), "no section at byte {byte}");
        &section.unwrap_or_else(|| unreachable!()).format
    }

    fn hidden(format: &TextFormat) -> bool {
        format.font_id.size == HIDDEN_FONT_SIZE && format.color == Color32::TRANSPARENT
    }

    fn visible(format: &TextFormat) -> bool {
        format.font_id.size > 5.0 && format.color != Color32::TRANSPARENT
    }

    fn galley(ctx: &egui::Context, job: LayoutJob) -> Arc<egui::Galley> {
        ctx.fonts_mut(|fonts| fonts.layout_job(job))
    }

    #[test]
    fn marks_are_hidden_off_the_caret_line_and_shown_on_it() {
        let ctx = ctx();
        let text = "## Title\nsome **bold** and *it* ~~gone~~\n";
        let off = job(&ctx, text, None);
        assert!(hidden(format_at(&off, "## ", 0)));
        assert!(hidden(format_at(&off, "## ", 2)), "the space after ##");
        assert!(visible(format_at(&off, "Title", 0)));
        assert!(hidden(format_at(&off, "**bold", 0)));
        assert!(hidden(format_at(&off, "bold**", 4)));
        assert!(visible(format_at(&off, "bold", 0)));
        assert!(hidden(format_at(&off, "*it*", 0)));
        assert!(hidden(format_at(&off, "~~gone", 0)));

        // Caret on the heading: its marks show (weak), the body's stay hidden.
        let heading = job(&ctx, text, Some((0, 0)));
        let mark = format_at(&heading, "## ", 0);
        assert!(visible(mark));
        assert_eq!(mark.color, ctx.style().visuals.weak_text_color());
        assert_eq!(
            mark.font_id.size,
            format_at(&heading, "Title", 0).font_id.size,
            "the marks keep the heading size"
        );
        assert!(hidden(format_at(&heading, "**bold", 0)));

        // Selection over both lines: everything shows.
        let both = job(&ctx, text, Some((0, 1)));
        for snippet in ["## ", "**bold", "*it*", "~~gone"] {
            assert!(visible(format_at(&both, snippet, 0)), "{snippet}");
        }
    }

    #[test]
    fn headings_use_the_preview_sizes_colours_and_font() {
        let ctx = ctx();
        let style = style(&ctx);
        let text = "# A\n## B\n### C\n#### D\n##### E\n###### F\nbody\n";
        let job = job(&ctx, text, None);
        for (level, snippet) in ["A", "B", "C", "D", "E", "F"].iter().enumerate() {
            let format = format_at(&job, &format!("{snippet}\n"), 0);
            let expected = style.body_size * crate::md::HEADING_FONT_SCALES[level];
            assert_eq!(
                format.font_id,
                FontId::new(expected, crate::ui_style::bold_family()),
                "H{} (bold)",
                level + 1
            );
            assert_eq!(
                format.color,
                crate::md::heading_color(&ctx.style().visuals, level + 1, true)
            );
        }
        // Body text has the Body style's size, as the preview.
        let body = format_at(&job, "body", 0);
        let body_font = egui::TextStyle::Body.resolve(&ctx.style());
        assert_eq!(body.font_id, FontId::proportional(body_font.size));
        assert_eq!(style.body_size, body_font.size);
        assert_eq!(body.color, ctx.style().visuals.text_color());
    }

    #[test]
    fn links_keep_their_text_and_hide_the_url() {
        let ctx = ctx();
        let visuals = ctx.style().visuals.clone();
        let text = "See [Rust](https://rust-lang.org) and ![logo](img.png) or <https://a.b>.\n";
        let off = job(&ctx, text, None);
        let link = format_at(&off, "Rust]", 0);
        assert!(visible(link));
        assert_eq!(link.color, visuals.hyperlink_color);
        assert_ne!(link.underline, Stroke::NONE);
        for mark in [
            "[Rust",
            "](https",
            "rust-lang.org)",
            "![logo",
            "](img.png)",
            "<https",
            ">.",
        ] {
            assert!(hidden(format_at(&off, mark, 0)), "{mark}");
        }
        let alt = format_at(&off, "logo", 0);
        assert!(visible(alt));
        assert_eq!(alt.color, visuals.weak_text_color());
        assert!(visible(format_at(&off, "https://a.b", 0)), "autolink text");

        let on = job(&ctx, text, Some((0, 0)));
        assert!(visible(format_at(&on, "](https", 0)));
    }

    #[test]
    fn inline_code_strong_emphasis_and_strike_are_styled() {
        let ctx = ctx();
        let style = style(&ctx);
        let text = "a `code` b **strong** c *em* d ~~del~~ e ``x`y``\n";
        let job = job(&ctx, text, None);
        let code = format_at(&job, "code", 0);
        assert_eq!(code.font_id.family, FontFamily::Monospace);
        assert_eq!(code.font_id.size, style.body_size * CODE_FONT_SCALE);
        assert_eq!(code.background, style.code_bg);
        assert!(hidden(format_at(&job, "`code", 0)));
        assert!(hidden(format_at(&job, "``x", 0)));
        assert!(hidden(format_at(&job, "``x", 1)));
        assert!(
            visible(format_at(&job, "x`y", 1)),
            "a backtick inside the span"
        );
        assert_eq!(
            format_at(&job, "strong", 0).font_id.family,
            crate::ui_style::bold_family()
        );
        assert!(format_at(&job, "em*", 0).italics);
        assert_ne!(format_at(&job, "del", 0).strikethrough, Stroke::NONE);
        // Plain text is untouched.
        let plain = format_at(&job, " b ", 0);
        assert_eq!(plain.font_id, FontId::proportional(style.body_size));
        assert!(!plain.italics);
    }

    #[test]
    fn hidden_marks_keep_one_glyph_per_char_and_take_no_room() {
        let ctx = ctx();
        let url = "x".repeat(400);
        let text = format!("## Heading\n[link]({url}) **b** `c`\n> quote\n");
        let analysis = analyze(&text);
        let style = style(&ctx);
        let hidden_galley = galley(
            &ctx,
            layout_job(&text, &analysis, None, &style, measure(&ctx)),
        );
        let shown_galley = galley(
            &ctx,
            layout_job(&text, &analysis, Some((0, 3)), &style, measure(&ctx)),
        );
        let chars = text.chars().count();
        for galley in [&hidden_galley, &shown_galley] {
            assert_eq!(galley.end().index, chars, "one glyph per char");
            // Caret positions never go backwards along a row.
            let mut last = (f32::NEG_INFINITY, f32::NEG_INFINITY);
            for index in 0..=chars {
                let rect = galley.pos_from_cursor(egui::text::CCursor::new(index));
                assert!(rect.min.x.is_finite() && rect.min.y.is_finite());
                if rect.min.y == last.1 {
                    assert!(rect.min.x >= last.0, "char {index}");
                }
                last = (rect.min.x, rect.min.y);
            }
        }
        // The 401-char hidden URL is narrower than a pixel...
        let link_line = text.find("[link]").unwrap_or_default();
        let url_start = text[..link_line + 7].chars().count();
        let x = |galley: &egui::Galley, char_index: usize| {
            galley
                .pos_from_cursor(egui::text::CCursor::new(char_index))
                .min
                .x
        };
        let url_width = x(&hidden_galley, url_start + url.len() + 1) - x(&hidden_galley, url_start);
        assert!(url_width < 0.5, "{url_width}");
        // ...and "link" starts at the left edge when `[` is hidden.
        let link_x = x(&hidden_galley, url_start - 6);
        assert!(link_x < 0.5, "{link_x}");
        // A click at the left edge of a hidden heading lands before the marks
        // (the editor then moves it after them).
        let heading_row = hidden_galley.rows[0].rect();
        let at_edge = hidden_galley.cursor_from_pos(egui::vec2(0.0, heading_row.center().y));
        assert_eq!(at_edge.index, 0);
        // Revealed, the URL takes real room.
        let shown = x(&shown_galley, url_start + url.len() + 1) - x(&shown_galley, url_start);
        assert!(shown > 400.0, "{shown}");
    }

    #[test]
    fn tiny_font_sizes_are_not_clamped() {
        // egui lays out any positive size (no clamping): the width of 1000
        // marks shrinks with the size, and every char keeps a glyph and a
        // caret position, down to sizes far below `HIDDEN_FONT_SIZE`.
        let ctx = ctx();
        let marks = "*".repeat(1000);
        let text = format!("a{marks}b");
        let mut widths = Vec::new();
        for size in [1.0, 0.1, 0.01, 0.001, HIDDEN_FONT_SIZE, 1e-6] {
            let mut job = LayoutJob::default();
            let body = FontId::proportional(15.0);
            job.append("a", 0.0, TextFormat::simple(body.clone(), Color32::WHITE));
            job.append(
                &marks,
                0.0,
                TextFormat::simple(FontId::proportional(size), Color32::TRANSPARENT),
            );
            job.append("b", 0.0, TextFormat::simple(body, Color32::WHITE));
            let galley = galley(&ctx, job);
            assert_eq!(galley.end().index, text.chars().count(), "size {size}");
            assert_eq!(galley.rows.len(), 1);
            let x = |index: usize| {
                galley
                    .pos_from_cursor(egui::text::CCursor::new(index))
                    .min
                    .x
            };
            for index in 0..=1002 {
                assert!(x(index).is_finite(), "size {size}, char {index}");
            }
            // Clicks map back to chars: before the marks, after them.
            let row_y = galley.rows[0].rect().center().y;
            let after_a = galley.cursor_from_pos(egui::vec2(x(1) - 0.1, row_y)).index;
            assert!(after_a <= 1, "size {size}");
            let on_b = galley
                .cursor_from_pos(egui::vec2(x(1001) + 1.0, row_y))
                .index;
            assert_eq!(on_b, 1001, "size {size}");
            assert!(
                galley.rows[0].height() >= 15.0,
                "the marks do not shrink the row"
            );
            widths.push(x(1001) - x(1));
        }
        assert!(widths[0] > 300.0, "{widths:?}");
        assert!(
            widths.windows(2).all(|pair| pair[1] <= pair[0]),
            "{widths:?}"
        );
        assert!(widths[2] > 1.0, "0.01 is not small enough, {widths:?}");
        assert!(
            widths[4] < 1.0,
            "1000 hidden marks: under a point, {widths:?}"
        );
    }

    #[test]
    fn line_breaks_and_empty_lines_keep_body_rows() {
        let ctx = ctx();
        let style = style(&ctx);
        for text in ["# Title\n\nbody\n", "# Title\r\n\r\nbody\r\n", "**x**\n"] {
            let analysis = analyze(text);
            let galley = galley(
                &ctx,
                layout_job(text, &analysis, None, &style, measure(&ctx)),
            );
            assert_eq!(galley.end().index, text.chars().count());
            // The empty last line (after the final break) is a body row.
            let last = galley
                .rows
                .last()
                .map(|row| row.height())
                .unwrap_or_default();
            assert!(last >= style.body_row - 1.0, "{text:?}: {last}");
        }
        let job = job(&ctx, "# Title\n", None);
        assert!(visible(format_at(&job, "\n", 0)) || format_at(&job, "\n", 0).font_id.size > 5.0);
    }

    #[test]
    fn lists_hide_bullets_behind_overlays() {
        let ctx = ctx();
        let style = style(&ctx);
        let text = "- one\n  - two\n- [ ] todo\n- [x] done\n1. first\n";
        let analysis = analyze(text);
        let job = layout_job(text, &analysis, None, &style, measure(&ctx));
        // `- ` is invisible but keeps its width; ordered markers stay.
        let bullet = format_at(&job, "- one", 0);
        assert_eq!(bullet.color, Color32::TRANSPARENT);
        assert_eq!(bullet.font_id, FontId::monospace(style.body_size));
        assert!(
            hidden(format_at(&job, "- [ ]", 0)),
            "the checkbox replaces the bullet"
        );
        assert_eq!(format_at(&job, "[ ] todo", 0).color, Color32::TRANSPARENT);
        assert!(visible(format_at(&job, "1. first", 0)));
        assert_eq!(
            format_at(&job, "  - two", 0).font_id.family,
            FontFamily::Monospace,
            "list indentation"
        );

        let galley = galley(&ctx, job);
        let overlays = place_overlays(&galley, &analysis, None, &style);
        let kinds: Vec<_> = overlays.iter().map(|overlay| overlay.kind).collect();
        let todo = text.find("[ ]").unwrap_or_default() + 1;
        let done = text.find("[x]").unwrap_or_default() + 1;
        assert_eq!(
            kinds,
            [
                OverlayKind::Bullet("\u{2022}"),
                OverlayKind::Bullet("\u{25E6}"),
                OverlayKind::Checkbox {
                    checked: false,
                    toggle_byte: todo
                },
                OverlayKind::Checkbox {
                    checked: true,
                    toggle_byte: done
                },
            ]
        );
        // Each overlay sits on its own line's row, over the hidden marker.
        for (overlay, row) in overlays.iter().zip(&galley.rows) {
            assert_eq!(overlay.rect.y_range(), row.rect().y_range());
            assert!(overlay.rect.width() > 1.0);
        }
        assert!(
            overlays[1].rect.left() > overlays[0].rect.left(),
            "nested bullet indented"
        );

        // Caret on the first line: its bullet shows as text, no overlay.
        let revealed = place_overlays(
            &galley_for(&ctx, text, &analysis, Some((0, 0)), &style),
            &analysis,
            Some((0, 0)),
            &style,
        );
        assert_eq!(revealed.len(), 3);
    }

    fn galley_for(
        ctx: &egui::Context,
        text: &str,
        analysis: &Analysis,
        reveal: Reveal,
        style: &FormatStyle,
    ) -> Arc<egui::Galley> {
        galley(ctx, layout_job(text, analysis, reveal, style, measure(ctx)))
    }

    #[test]
    fn quotes_rules_and_code_blocks_get_overlays_at_their_rows() {
        let ctx = ctx();
        let style = style(&ctx);
        let text = "> quoted\n> > nested\n\n---\n\n```rust\nfn main() {}\n```\nafter\n";
        let analysis = analyze(text);
        let job = layout_job(text, &analysis, None, &style, measure(&ctx));
        assert!(hidden(format_at(&job, "> quoted", 0)));
        assert!(hidden(format_at(&job, "> > nested", 2)));
        assert!(hidden(format_at(&job, "---", 0)));
        assert!(hidden(format_at(&job, "```rust", 0)));
        let info = format_at(&job, "rust\n", 0);
        assert!(visible(info));
        assert_eq!(info.color, style.weak);
        assert_eq!(
            format_at(&job, "fn main", 0).font_id.family,
            FontFamily::Monospace
        );
        assert!(hidden(format_at(&job, "```\nafter", 0)));
        // Quote content is indented by its depth.
        let quoted = job
            .sections
            .iter()
            .find(|section| section.byte_range.start == 0)
            .map(|section| section.leading_space);
        assert_eq!(quoted, Some(style.quote_indent()));

        let galley = galley(&ctx, job);
        let rows: Vec<Rect> = galley
            .rows
            .iter()
            .map(egui::epaint::text::PlacedRow::rect)
            .collect();
        let overlays = place_overlays(&galley, &analysis, None, &style);
        let of = |kind: OverlayKind| -> Vec<Rect> {
            overlays
                .iter()
                .filter(|overlay| overlay.kind == kind)
                .map(|overlay| overlay.rect)
                .collect()
        };
        let bars = of(OverlayKind::QuoteBar);
        assert_eq!(bars.len(), 3, "one bar on line 1, two on line 2");
        assert_eq!(bars[0].y_range(), rows[0].y_range());
        assert_eq!(bars[2].y_range(), rows[1].y_range());
        assert!(bars[2].left() > bars[1].left());
        let rule = of(OverlayKind::Rule);
        assert_eq!(rule.len(), 1);
        assert_eq!(rule[0].y_range(), rows[3].y_range());
        assert!(
            rows[3].height() >= style.body_row - 1.0,
            "a rule keeps a body row"
        );
        let code = of(OverlayKind::CodeBlock);
        assert_eq!(code.len(), 1);
        assert_eq!(code[0].top(), rows[5].top(), "from the opening fence");
        assert_eq!(code[0].bottom(), rows[7].bottom(), "to the closing fence");
        assert!(
            rows[7].height() < style.body_row,
            "a hidden fence is a thin row"
        );

        // Revealed quote line: its `> ` shows where the bar was (no indent,
        // no bar); the other quote line keeps its bars.
        let revealed_job = layout_job(text, &analysis, Some((0, 0)), &style, measure(&ctx));
        let mark = format_at(&revealed_job, "> quoted", 0);
        assert!(visible(mark));
        assert_eq!(revealed_job.sections[0].leading_space, 0.0);
        let revealed = place_overlays(
            &self::galley(&ctx, revealed_job),
            &analysis,
            Some((0, 0)),
            &style,
        );
        assert_eq!(
            revealed
                .iter()
                .filter(|overlay| overlay.kind == OverlayKind::QuoteBar)
                .count(),
            2
        );
    }

    #[test]
    fn mermaid_blocks_make_room_for_their_picture() {
        let ctx = ctx();
        let text = "intro\n\n```mermaid\ngraph TD\n  A --> B\n```\n\nafter\n";
        let analysis = analyze(text);
        let diagrams = analysis.diagrams();
        assert_eq!(diagrams.len(), 1);
        assert_eq!(&text[diagrams[0].code.clone()], "graph TD\n  A --> B\n");
        // The caret on any of its lines reveals the whole block (its code).
        let code_line = text.find("A -->").unwrap_or_default();
        let reveal = analysis.reveal_for(code_line, code_line);
        assert_eq!(reveal, Some((2, 5)));
        assert!(diagrams[0].revealed(reveal));
        assert!(!diagrams[0].revealed(None));
        // Off the caret, with a picture 120 points tall: the first code row
        // is that tall, the next one has no height.
        let mut job = job(&ctx, text, None);
        hide_diagrams(&mut job.sections, &analysis, &[(0, 120.0)]);
        let rows = galley(&ctx, job);
        let top = diagram_top(&rows, &analysis, 0);
        assert!(top.is_some());
        let heights: Vec<f32> = rows.rows.iter().map(|row| row.height()).collect();
        assert!(
            heights.iter().any(|&height| (height - 120.0).abs() < 0.5),
            "{heights:?}"
        );
        assert!(format_at_hidden(&rows, text, "A -->"));
    }

    /// Whether the char at `snippet` is laid out hidden.
    fn format_at_hidden(galley: &egui::Galley, text: &str, snippet: &str) -> bool {
        let byte = text.find(snippet).unwrap_or_default();
        galley
            .job
            .sections
            .iter()
            .find(|section| section.byte_range.contains(&byte))
            .is_some_and(|section| hidden(&section.format))
    }

    #[test]
    fn tables_line_up_as_a_grid() {
        let ctx = ctx();
        let text = "| a | **b** |\n|---|:-:|\n| `ccc` | d |\n\n<div>*x*</div>\n";
        let analysis = analyze(text);
        let format_style = style(&ctx);
        let x_of = |galley: &egui::Galley, snippet: &str, nth: usize| {
            let byte = text.match_indices(snippet).nth(nth).map_or(0, |(at, _)| at);
            let char_index = text[..byte].chars().count();
            galley
                .pos_from_cursor(egui::text::CCursor::new(char_index))
                .min
                .x
        };

        // Off the caret: pipes and the `|---|` row hidden, cell text in the
        // body font (header bold), columns lined up, grid lines drawn.
        let off = layout_job(text, &analysis, None, &format_style, measure(&ctx));
        assert!(hidden(format_at(&off, "| a", 0)));
        assert!(hidden(format_at(&off, "|---", 1)));
        assert!(visible(format_at(&off, "a |", 0)));
        assert_eq!(
            format_at(&off, "a |", 0).font_id.family,
            crate::ui_style::bold_family()
        );
        assert_eq!(
            format_at(&off, "d |", 0).font_id.family,
            FontFamily::Proportional
        );
        assert!(visible(format_at(&off, "<div>", 0)), "HTML stays raw");
        let off_galley = galley(&ctx, off);
        assert!((x_of(&off_galley, "ccc", 0) - x_of(&off_galley, "a ", 0)).abs() <= 1.0);
        // Second column (centred): both cells' centres line up.
        let mid = |snippet: &str, len: usize| {
            let start = x_of(&off_galley, snippet, 0);
            let at = text.find(snippet).unwrap_or_default() + len;
            let char_index = text[..at].chars().count();
            let end = off_galley
                .pos_from_cursor(egui::text::CCursor::new(char_index))
                .min
                .x;
            f32::midpoint(start, end)
        };
        assert!((mid("b**", 1) - mid("d |", 1)).abs() <= 1.0);
        let overlays = place_overlays(&off_galley, &analysis, None, &format_style);
        for kind in [
            OverlayKind::TableBorder,
            OverlayKind::TableHeader,
            OverlayKind::TableLine,
        ] {
            assert!(
                overlays.iter().any(|overlay| overlay.kind == kind),
                "{kind:?}"
            );
        }

        // The caret in the table: all of it shows its source, lined up.
        let reveal = analysis.reveal_for(text.find("d |").unwrap_or_default(), 0);
        assert_eq!(reveal, Some((0, 2)));
        let on = layout_job(text, &analysis, reveal, &format_style, measure(&ctx));
        assert!(visible(format_at(&on, "| a", 0)));
        assert!(visible(format_at(&on, "|---", 1)));
        let on_galley = galley(&ctx, on);
        assert!((x_of(&on_galley, "|", 1) - x_of(&on_galley, "|", 7)).abs() <= 1.0);
        let overlays = place_overlays(&on_galley, &analysis, reveal, &format_style);
        assert!(
            overlays
                .iter()
                .all(|overlay| overlay.kind != OverlayKind::TableBorder)
        );
    }

    #[test]
    fn reveal_and_line_prefix_lookups() {
        // The blank line ends the quote ("plain" right after the quote would
        // be a lazy continuation of it).
        let text = "## Head\n> - [ ] task\n\nplain\n";
        let analysis = analyze(text);
        assert_eq!(analysis.reveal_for(0, 0), Some((0, 0)));
        assert_eq!(
            analysis.reveal_for(7, 7),
            Some((0, 0)),
            "before the line break"
        );
        assert_eq!(analysis.reveal_for(8, 8), Some((1, 1)));
        assert_eq!(
            analysis.reveal_for(30, 2),
            Some((0, 4)),
            "unordered, to the end"
        );
        // Plain lines have nothing to reveal.
        assert_eq!(analysis.reveal_for(22, 24), None);
        assert_eq!(analysis.reveal_for(21, 27), None, "blank and plain");
        assert_eq!(analysis.reveal_for(28, 28), None, "the empty last line");
        assert_eq!(analysis.reveal_for(15, 24), Some((1, 3)));
        assert_eq!(analysis.after_line_prefix(0), Some(3));
        assert_eq!(analysis.after_line_prefix(3), None);
        // `> - [ ] ` is all prefix.
        assert_eq!(analysis.after_line_prefix(8), Some(8 + 8));
        assert_eq!(analysis.after_line_prefix(22), None, "plain line");
        assert_eq!(analyze("").reveal_for(0, 0), None);
        assert_eq!(
            analyze("**b**").reveal_for(5, 5),
            Some((0, 0)),
            "no final break"
        );
    }

    #[test]
    fn analysis_is_stable_on_awkward_input() {
        let ctx = ctx();
        let style = style(&ctx);
        for text in [
            "",
            "\n",
            "#",
            "# ",
            "**",
            "[",
            "```",
            "```\ncode",
            "- ",
            "-",
            "> ",
            ">",
            "* * *",
            "Title\n=====\n",
            "é **ü** `ß` [ö](ä)\r\n",
            "\t- tab\n",
            "1) x\n   - [X] y\n",
            include_str!("bundled/demo.md"),
            include_str!("bundled/verification.md"),
        ] {
            let analysis = analyze(text);
            let job = layout_job(text, &analysis, None, &style, measure(&ctx));
            let covered: usize = job.sections.iter().map(|s| s.byte_range.len()).sum();
            assert_eq!(covered, text.len(), "{text:?}");
            let galley = galley(&ctx, job);
            assert_eq!(galley.end().index, text.chars().count());
            let _ = place_overlays(&galley, &analysis, Some((0, 0)), &style);
        }
    }
}
