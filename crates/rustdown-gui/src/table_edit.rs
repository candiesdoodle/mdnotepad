//! Table editing in the editor: the right-click menu's Insert / Delete row
//! and column, Tab and Shift+Tab between cells (Tab in the last cell adds
//! a row), and Enter for a new row below (Enter in an empty last row leaves
//! the table).  A change rewrites the whole table with its cells padded,
//! so the source stays lined up, as one undoable edit.

use std::ops::Range;

use eframe::egui::{self, Key, Modifiers};
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag};

use crate::{RustdownApp, edit_ops, menu};

/// A table of the text, parsed for editing.  Lines: 0 is the header, 1
/// the `|---|` row, then the body rows.
#[derive(Clone, Debug)]
pub(crate) struct Table {
    /// Its byte range: whole lines, without the last line break.
    range: Range<usize>,
    /// What comes before each line's cells (quote marks, indentation),
    /// from its first line.
    prefix: String,
    header: Vec<String>,
    align: Vec<Alignment>,
    rows: Vec<Vec<String>>,
    /// Each line's byte range and its cells' byte ranges (between pipes).
    lines: Vec<(Range<usize>, Vec<Range<usize>>)>,
}

/// A change from the right-click menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableOp {
    InsertRowAbove,
    InsertRowBelow,
    InsertColumnLeft,
    InsertColumnRight,
    DeleteRow,
    DeleteColumn,
}

impl TableOp {
    const ALL: [Self; 6] = [
        Self::InsertRowAbove,
        Self::InsertRowBelow,
        Self::InsertColumnLeft,
        Self::InsertColumnRight,
        Self::DeleteRow,
        Self::DeleteColumn,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::InsertRowAbove => "Insert row above",
            Self::InsertRowBelow => "Insert row below",
            Self::InsertColumnLeft => "Insert column left",
            Self::InsertColumnRight => "Insert column right",
            Self::DeleteRow => "Delete row",
            Self::DeleteColumn => "Delete column",
        }
    }
}

/// A key the editor hands to tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableKey {
    /// Tab: the next cell.
    Next,
    /// Shift+Tab: the previous cell.
    Previous,
    /// Enter: a new row below.
    NewRow,
}

/// What a key does in a table: select bytes of the text, or replace a
/// range with new text and select bytes of the result.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeyAction {
    Select(Range<usize>),
    Edit {
        range: Range<usize>,
        text: String,
        select: Range<usize>,
    },
}

/// Length of the quote marks and indentation at the start of `line`.
fn prefix_len(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t', '>']).len()
}

/// The cells of a table line at byte `at`: the byte ranges between its
/// pipes (`\|` is not one), without the empty ones outside a leading and
/// a closing pipe.
fn cells_of(at: usize, line: &str) -> Vec<Range<usize>> {
    let body = prefix_len(line);
    let bytes = line.as_bytes();
    let mut cells = Vec::new();
    let mut start = body;
    for index in body..bytes.len() {
        if bytes[index] == b'|' && (index == 0 || bytes[index - 1] != b'\\') {
            cells.push(start..index);
            start = index + 1;
        }
    }
    cells.push(start..bytes.len());
    if line[body..].starts_with('|') && cells.len() > 1 {
        cells.remove(0);
    }
    if cells.len() > 1
        && line.trim_end().ends_with('|')
        && cells
            .last()
            .is_some_and(|last| line[last.clone()].trim().is_empty())
    {
        cells.pop();
    }
    cells
        .into_iter()
        .map(|cell| at + cell.start..at + cell.end)
        .collect()
}

/// The text of `row`'s cell `column` (empty past its end).
fn cell(row: &[String], column: usize) -> &str {
    row.get(column).map_or("", String::as_str)
}

/// A column's alignment from its `|---|` cell.
fn align_of(cell: &str) -> Alignment {
    match (cell.starts_with(':'), cell.len() > 1 && cell.ends_with(':')) {
        (true, true) => Alignment::Center,
        (true, false) => Alignment::Left,
        (false, true) => Alignment::Right,
        (false, false) => Alignment::None,
    }
}

/// The `|---|` cell of a column `width` chars wide.
fn dashes(align: Alignment, width: usize) -> String {
    match align {
        Alignment::Left => format!(":{}", "-".repeat(width - 1)),
        Alignment::Right => format!("{}:", "-".repeat(width - 1)),
        Alignment::Center => format!(":{}:", "-".repeat(width - 2)),
        Alignment::None => "-".repeat(width),
    }
}

/// The table holding byte `byte` of `text`.
#[must_use]
pub(crate) fn find(text: &str, byte: usize) -> Option<Table> {
    let byte = byte.min(text.len());
    let line_start = text[..byte].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[byte..].find('\n').map_or(text.len(), |at| byte + at);
    if !text[line_start..line_end].contains('|') {
        return None;
    }
    let range = Parser::new_ext(text, Options::ENABLE_TABLES)
        .into_offset_iter()
        .find_map(|(event, range)| match event {
            Event::Start(Tag::Table(_)) if range.start <= line_end && line_start < range.end => {
                Some(range)
            }
            _ => None,
        })?;
    let start = text[..range.start].rfind('\n').map_or(0, |at| at + 1);
    let mut end = range.end.min(text.len());
    while end > start && matches!(text.as_bytes()[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    let mut lines = Vec::new();
    let mut at = start;
    for line in text[start..end].split('\n') {
        let content = line.strip_suffix('\r').unwrap_or(line);
        lines.push((at..at + content.len(), cells_of(at, content)));
        at += line.len() + 1;
    }
    if lines.len() < 2 {
        return None;
    }
    let first = &text[lines[0].0.clone()];
    let prefix = first[..prefix_len(first)].to_owned();
    let cell_texts = |index: usize| -> Vec<String> {
        lines[index]
            .1
            .iter()
            .map(|cell| text[cell.clone()].trim().to_owned())
            .collect()
    };
    let header = cell_texts(0);
    let align = cell_texts(1).iter().map(|cell| align_of(cell)).collect();
    let rows = (2..lines.len()).map(cell_texts).collect();
    Some(Table {
        range: start..end,
        prefix,
        header,
        align,
        rows,
        lines,
    })
}

impl Table {
    /// The table's byte range in the text.
    #[must_use]
    pub(crate) fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    fn columns(&self) -> usize {
        let widest_row = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        self.header.len().max(widest_row).max(1)
    }

    /// The line and column of byte `byte`.
    #[must_use]
    pub(crate) fn cell_at(&self, byte: usize) -> (usize, usize) {
        let line = self
            .lines
            .iter()
            .position(|(range, _)| byte <= range.end)
            .unwrap_or(self.lines.len() - 1);
        let cells = &self.lines[line].1;
        let column = cells
            .iter()
            .position(|cell| byte <= cell.end)
            .unwrap_or_else(|| cells.len().saturating_sub(1));
        (line, column)
    }

    /// Whether `op` can be done on line `line`.
    #[must_use]
    pub(crate) fn allows(&self, op: TableOp, line: usize) -> bool {
        match op {
            TableOp::InsertRowAbove | TableOp::DeleteRow => line >= 2,
            TableOp::DeleteColumn => self.columns() > 1,
            TableOp::InsertRowBelow | TableOp::InsertColumnLeft | TableOp::InsertColumnRight => {
                true
            }
        }
    }

    /// The text of line `line` (0 header, 1 the `|---|` row).
    fn line_cells(&self, line: usize) -> Option<&Vec<String>> {
        match line {
            0 => Some(&self.header),
            1 => None,
            _ => self.rows.get(line - 2),
        }
    }

    /// The table as text, its lines joined by `eol`, and where each cell's
    /// text is in it: `[line][column]` byte ranges.
    fn render(&self, eol: &str) -> (String, Vec<Vec<Range<usize>>>) {
        let columns = self.columns();
        let mut widths = vec![3usize; columns];
        for row in std::iter::once(&self.header).chain(&self.rows) {
            for (column, width) in widths.iter_mut().enumerate() {
                *width = (*width).max(cell(row, column).chars().count());
            }
        }
        let delimiter: Vec<String> = (0..columns)
            .map(|column| {
                let align = self.align.get(column).copied().unwrap_or(Alignment::None);
                dashes(align, widths[column])
            })
            .collect();
        let mut out = String::new();
        let mut spots = Vec::with_capacity(self.rows.len() + 2);
        let lines = std::iter::once(&self.header)
            .chain(std::iter::once(&delimiter))
            .chain(&self.rows);
        for (index, row) in lines.enumerate() {
            if index > 0 {
                out.push_str(eol);
            }
            out.push_str(&self.prefix);
            out.push('|');
            let mut line_spots = Vec::with_capacity(columns);
            for (column, width) in widths.iter().enumerate() {
                let text = cell(row, column);
                out.push(' ');
                line_spots.push(out.len()..out.len() + text.len());
                out.push_str(text);
                for _ in text.chars().count()..*width {
                    out.push(' ');
                }
                out.push_str(" |");
            }
            spots.push(line_spots);
        }
        (out, spots)
    }

    /// The table rewritten after `op` at cell (`line`, `column`), and the
    /// byte range (in the new text) of the cell the caret goes to.
    #[must_use]
    pub(crate) fn apply(
        &self,
        op: Option<TableOp>,
        line: usize,
        column: usize,
        eol: &str,
    ) -> (String, Range<usize>) {
        let mut table = self.clone();
        let columns = table.columns();
        let empty_row = || vec![String::new(); columns];
        let body_row = line.checked_sub(2);
        let (line, column) = match op {
            None => (line, column),
            Some(TableOp::InsertRowAbove) => {
                let at = body_row.unwrap_or(0);
                table.rows.insert(at, empty_row());
                (at + 2, column)
            }
            Some(TableOp::InsertRowBelow) => {
                let at = body_row.map_or(0, |row| row + 1).min(table.rows.len());
                table.rows.insert(at, empty_row());
                (at + 2, column)
            }
            Some(TableOp::InsertColumnLeft | TableOp::InsertColumnRight) => {
                let at = (if op == Some(TableOp::InsertColumnLeft) {
                    column
                } else {
                    column + 1
                })
                .min(columns);
                for row in std::iter::once(&mut table.header).chain(&mut table.rows) {
                    row.resize(columns, String::new());
                    row.insert(at, String::new());
                }
                table.align.resize(columns, Alignment::None);
                table.align.insert(at, Alignment::None);
                (line, at)
            }
            Some(TableOp::DeleteRow) => {
                if let Some(row) = body_row
                    && row < table.rows.len()
                {
                    table.rows.remove(row);
                }
                let line = if table.rows.is_empty() {
                    0
                } else {
                    line.min(table.rows.len() + 1)
                };
                (line, column)
            }
            Some(TableOp::DeleteColumn) => {
                if columns > 1 {
                    let at = column.min(columns - 1);
                    for row in std::iter::once(&mut table.header).chain(&mut table.rows) {
                        if at < row.len() {
                            row.remove(at);
                        }
                    }
                    if at < table.align.len() {
                        table.align.remove(at);
                    }
                }
                (line, column.min(table.columns() - 1))
            }
        };
        let (text, spots) = table.render(eol);
        let spot = spots
            .get(line)
            .and_then(|cells| cells.get(column.min(cells.len().saturating_sub(1))))
            .cloned()
            .unwrap_or(0..0);
        (text, spot)
    }

    /// The byte range of the text of cell (`line`, `column`) as it is now,
    /// if the line has that cell.
    fn cell_text(&self, text: &str, line: usize, column: usize) -> Option<Range<usize>> {
        let cell = self.lines.get(line)?.1.get(column)?.clone();
        let inner = &text[cell.clone()];
        let start = cell.start + (inner.len() - inner.trim_start().len());
        let end = (cell.end - (inner.len() - inner.trim_end().len())).max(start);
        Some(start..end)
    }

    /// A key at byte `caret` of `text` (this table's text).
    fn on_key(&self, text: &str, caret: usize, key: TableKey, eol: &str) -> KeyAction {
        let (line, column) = self.cell_at(caret);
        let columns = self.columns();
        let last_line = self.lines.len() - 1;
        let edit = |op: Option<TableOp>, line: usize, column: usize| {
            let (new, spot) = self.apply(op, line, column, eol);
            KeyAction::Edit {
                range: self.range(),
                text: new,
                select: self.range.start + spot.start..self.range.start + spot.end,
            }
        };
        let go_to = |line: usize, column: usize| {
            self.cell_text(text, line, column)
                .map_or_else(|| edit(None, line, column), KeyAction::Select)
        };
        match key {
            TableKey::Next => {
                if column + 1 < columns {
                    go_to(line, column + 1)
                } else if line < last_line {
                    // Past the `|---|` row from the header.
                    go_to((if line == 0 { 2 } else { line + 1 }).min(last_line), 0)
                } else {
                    edit(Some(TableOp::InsertRowBelow), line, 0)
                }
            }
            TableKey::Previous => {
                if column > 0 {
                    go_to(line, column - 1)
                } else if line > 0 {
                    go_to(if line == 2 { 0 } else { line - 1 }, columns - 1)
                } else {
                    go_to(0, 0)
                }
            }
            TableKey::NewRow => {
                let empty_last = line >= 2
                    && line == last_line
                    && self
                        .line_cells(line)
                        .is_some_and(|cells| cells.iter().all(String::is_empty));
                if empty_last {
                    // Leave the table: without that row, then a blank line.
                    let (new, _) = self.apply(Some(TableOp::DeleteRow), line, 0, eol);
                    let new = format!("{new}{eol}{eol}");
                    let caret = self.range.start + new.len();
                    KeyAction::Edit {
                        range: self.range(),
                        text: new,
                        select: caret..caret,
                    }
                } else {
                    edit(Some(TableOp::InsertRowBelow), line, 0)
                }
            }
        }
    }
}

/// What `key` at byte `caret` of `text` does, when the caret is in a table.
#[must_use]
pub(crate) fn on_key(text: &str, caret: usize, key: TableKey, eol: &str) -> Option<KeyAction> {
    let table = find(text, caret)?;
    Some(table.on_key(text, caret, key, eol))
}

/// The table rows of the editor's right-click menu for `table`, clicked at
/// line `line`: the change chosen.
pub(crate) fn menu_section(ui: &mut egui::Ui, table: &Table, line: usize) -> Option<TableOp> {
    let mut chosen = None;
    for (index, op) in TableOp::ALL.into_iter().enumerate() {
        if index == 4 {
            menu::separator(ui);
        }
        let enabled = table.allows(op, line);
        let clicked = ui
            .add_enabled_ui(enabled, |ui| {
                menu::plain_row(ui, op.label(), menu::Mark::None)
            })
            .inner;
        if clicked {
            chosen = Some(op);
        }
    }
    chosen
}

impl RustdownApp {
    /// Tab, Shift+Tab and Enter in a table, before the editor reads them:
    /// whether one was taken.  `primary` is the caret's char index.
    pub(crate) fn table_keys(&mut self, ui: &egui::Ui, text: &str, primary: usize) -> bool {
        let pressed = ui.input(|input| {
            let tab = input.key_pressed(Key::Tab);
            if tab && input.modifiers.is_none() {
                Some((TableKey::Next, Modifiers::NONE, Key::Tab))
            } else if tab && input.modifiers.shift_only() {
                Some((TableKey::Previous, Modifiers::SHIFT, Key::Tab))
            } else if input.key_pressed(Key::Enter) && input.modifiers.is_none() {
                Some((TableKey::NewRow, Modifiers::NONE, Key::Enter))
            } else {
                None
            }
        });
        let Some((key, modifiers, logical)) = pressed else {
            return false;
        };
        let caret = edit_ops::char_to_byte(text, primary);
        let eol = self.tab().doc.line_ending.as_str();
        let Some(action) = on_key(text, caret, key, eol) else {
            return false;
        };
        if !ui.input_mut(|input| input.consume_key(modifiers, logical)) {
            return false;
        }
        match action {
            KeyAction::Select(bytes) => {
                let chars = edit_ops::byte_to_char(text, bytes.start)
                    ..edit_ops::byte_to_char(text, bytes.end);
                self.select_in_editor(ui.ctx(), chars);
            }
            KeyAction::Edit {
                range,
                text: with,
                select,
            } => self.replace_text_undoable(ui.ctx(), range, &with, select),
        }
        true
    }

    /// Do `op` on the table of the active tab at byte `byte` (where the
    /// right-click was), found again in case the text changed.
    pub(crate) fn apply_table_op(&mut self, ctx: &egui::Context, byte: usize, op: TableOp) {
        let tab = self.tab();
        let Some(table) = find(&tab.doc.text, byte) else {
            return;
        };
        let (line, column) = table.cell_at(byte);
        let eol = tab.doc.line_ending.as_str();
        let (new, spot) = table.apply(Some(op), line, column, eol);
        let start = table.range.start;
        self.replace_text_undoable(
            ctx,
            table.range(),
            &new,
            start + spot.start..start + spot.end,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str =
        "intro\n\n| Name | Age |\n|:-----|----:|\n| Ann | 30 |\n| Bob | 4 |\n\nafter\n";

    fn table() -> Table {
        let at = TABLE.find("Ann").unwrap_or_default();
        let found = find(TABLE, at);
        assert!(found.is_some());
        found.unwrap_or_else(|| unreachable!())
    }

    #[test]
    fn tables_are_found_with_their_cells() {
        let table = table();
        assert_eq!(
            &TABLE[table.range()],
            "| Name | Age |\n|:-----|----:|\n| Ann | 30 |\n| Bob | 4 |"
        );
        assert_eq!(table.header, ["Name", "Age"]);
        assert_eq!(table.align, [Alignment::Left, Alignment::Right]);
        assert_eq!(table.rows, [["Ann", "30"], ["Bob", "4"]]);
        assert_eq!(table.cell_at(TABLE.find("30").unwrap_or_default()), (2, 1));
        assert!(find(TABLE, 2).is_none(), "not in a table");
    }

    #[test]
    fn rows_and_columns_are_inserted_and_deleted_lined_up() {
        let table = table();
        let (text, spot) = table.apply(Some(TableOp::InsertRowBelow), 2, 1, "\n");
        assert_eq!(
            text,
            "| Name | Age |\n| :--- | --: |\n| Ann  | 30  |\n|      |     |\n| Bob  | 4   |"
        );
        assert_eq!(spot, 54..54, "caret in the new row's cell");
        let (text, spot) = table.apply(Some(TableOp::InsertColumnLeft), 0, 1, "\n");
        assert_eq!(
            text,
            "| Name |     | Age |\n| :--- | --- | --: |\n| Ann  |     | 30  |\n| Bob  |     | 4   |"
        );
        assert_eq!(text.as_bytes().get(spot.start), Some(&b' '));
        let (text, _) = table.apply(Some(TableOp::DeleteColumn), 3, 0, "\r\n");
        assert_eq!(text, "| Age |\r\n| --: |\r\n| 30  |\r\n| 4   |");
        let (text, _) = table.apply(Some(TableOp::DeleteRow), 3, 0, "\n");
        assert_eq!(text, "| Name | Age |\n| :--- | --: |\n| Ann  | 30  |");
        assert!(!table.allows(TableOp::DeleteRow, 0));
        assert!(!table.allows(TableOp::InsertRowAbove, 1));
    }

    #[test]
    fn tab_moves_between_cells_and_adds_a_row_at_the_end() {
        let at = |snippet: &str| TABLE.find(snippet).unwrap_or_default();
        let select = |snippet: &str| KeyAction::Select(at(snippet)..at(snippet) + snippet.len());
        assert_eq!(
            on_key(TABLE, at("Name"), TableKey::Next, "\n"),
            Some(select("Age"))
        );
        assert_eq!(
            on_key(TABLE, at("Age"), TableKey::Next, "\n"),
            Some(select("Ann"))
        );
        assert_eq!(
            on_key(TABLE, at("Ann"), TableKey::Previous, "\n"),
            Some(select("Age"))
        );
        let Some(KeyAction::Edit { text, .. }) = on_key(TABLE, at("4 |"), TableKey::Next, "\n")
        else {
            unreachable!("a new row");
        };
        assert!(text.ends_with("| Bob  | 4   |\n|      |     |"), "{text}");
    }

    #[test]
    fn enter_adds_a_row_and_leaves_from_an_empty_last_one() {
        let at = TABLE.find("Ann").unwrap_or_default();
        let Some(KeyAction::Edit { text, .. }) = on_key(TABLE, at, TableKey::NewRow, "\n") else {
            unreachable!("a new row");
        };
        assert!(
            text.contains("| Ann  | 30  |\n|      |     |\n| Bob"),
            "{text}"
        );
        let empty = "| a |\n|---|\n|   |\n";
        let Some(KeyAction::Edit { text, select, .. }) = on_key(empty, 13, TableKey::NewRow, "\n")
        else {
            unreachable!("leaves the table");
        };
        assert_eq!(text, "| a   |\n| --- |\n\n");
        assert_eq!(select, 17..17, "after the blank line");
    }
}
