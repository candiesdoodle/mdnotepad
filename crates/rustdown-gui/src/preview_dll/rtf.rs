//! Markdown as RTF for the preview pane's rich-text control: headings,
//! bold / italic / strikethrough, inline code and code blocks, links,
//! lists and task lists, quotes, tables and rules.  Images show as their
//! alt text (the preview loads nothing).

use std::fmt::Write as _;

use crate::parse::{Alignment, Block, ListItem, StyledText, TableData, parse_markdown_into};

/// Body text size, in half points.
const BODY_SIZE: u32 = 21;
/// Code size, in half points.
const CODE_SIZE: u32 = 19;
/// Heading sizes H1 to H6, in half points.
const HEADING_SIZES: [u32; 6] = [40, 32, 28, 24, 22, 21];
/// Indent per list or quote level, in twips.
const INDENT: u32 = 360;

/// The document's fonts and colours: 1 text, 2 links, 3 quotes and
/// rules, 4 code background, 5 table lines.
const HEADER: &str = "{\\rtf1\\ansi\\ansicpg1252\\deff0\\uc1\n\
{\\fonttbl{\\f0\\fswiss\\fcharset0 Segoe UI;}{\\f1\\fmodern\\fcharset0 Consolas;}}\n\
{\\colortbl;\\red27\\green27\\blue27;\\red11\\green92\\blue173;\\red110\\green110\\blue110;\
\\red240\\green240\\blue240;\\red200\\green200\\blue200;}\n\
\\viewkind4\\f0\\cf1";

/// `markdown` as an RTF document; tables are at most `width` twips wide.
#[must_use]
pub(crate) fn to_rtf(markdown: &str, width: u32) -> String {
    let mut blocks = Vec::new();
    parse_markdown_into(markdown, &mut blocks);
    let mut out = String::with_capacity(markdown.len() * 2 + HEADER.len());
    out.push_str(HEADER);
    let _ = writeln!(out, "\\fs{BODY_SIZE}");
    write_blocks(&mut out, &blocks, 0, width.max(2000));
    out.push('}');
    out
}

/// `text` with RTF's special characters escaped and everything outside
/// ASCII as `\uN?`.
fn push_text(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\n' => out.push_str("\\line "),
            '\t' => out.push_str("\\tab "),
            c if c.is_ascii_control() => {}
            c if c.is_ascii() => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{}?", unit.cast_signed());
                }
            }
        }
    }
}

/// `text` with its inline styles.
fn write_inline(out: &mut String, text: &StyledText) {
    let mut at = 0;
    for span in &text.spans {
        let start = (span.start as usize).min(text.text.len());
        let end = (span.end as usize).min(text.text.len());
        let (Some(gap), Some(piece)) = (text.text.get(at..start), text.text.get(start..end)) else {
            continue;
        };
        push_text(out, gap);
        let style = span.style;
        let mut controls = String::new();
        if style.strong() {
            controls.push_str("\\b");
        }
        if style.emphasis() {
            controls.push_str("\\i");
        }
        if style.strikethrough() {
            controls.push_str("\\strike");
        }
        if style.code() {
            let _ = write!(controls, "\\f1\\fs{CODE_SIZE}\\highlight4");
        }
        if style.has_link() {
            controls.push_str("\\cf2\\ul");
        }
        if controls.is_empty() {
            push_text(out, piece);
        } else {
            // The space ends the last control word; it is not text.
            out.push('{');
            out.push_str(&controls);
            out.push(' ');
            push_text(out, piece);
            out.push('}');
        }
        at = end;
    }
    push_text(out, text.text.get(at..).unwrap_or_default());
}

fn write_blocks(out: &mut String, blocks: &[Block], indent: u32, width: u32) {
    for block in blocks {
        write_block(out, block, indent, width);
    }
}

fn write_block(out: &mut String, block: &Block, indent: u32, width: u32) {
    match block {
        Block::Heading { level, text } => {
            let size = HEADING_SIZES[usize::from((*level).clamp(1, 6) - 1)];
            let _ = write!(
                out,
                "\\pard\\li{indent}\\sb240\\sa120\\keepn{{\\b\\fs{size} "
            );
            write_inline(out, text);
            out.push_str("}\\par\n");
        }
        Block::Paragraph(text) => {
            let _ = write!(out, "\\pard\\li{indent}\\sa120 ");
            write_inline(out, text);
            out.push_str("\\par\n");
        }
        Block::Code { code, .. } => {
            let _ = write!(
                out,
                "\\pard\\li{}\\sa120{{\\f1\\fs{CODE_SIZE}\\highlight4 ",
                indent + 120
            );
            push_text(out, code.trim_end_matches('\n'));
            out.push_str("}\\par\n");
        }
        Block::Quote(inner) => {
            out.push_str("{\\cf3 ");
            write_blocks(out, inner, indent + INDENT, width);
            out.push_str("}\n");
        }
        Block::UnorderedList(items) => write_list(out, items, None, indent, width),
        Block::OrderedList { start, items } => write_list(out, items, Some(*start), indent, width),
        Block::ThematicBreak => {
            let _ = writeln!(
                out,
                "\\pard\\li{indent}\\qc\\sa120{{\\cf5 \\emdash\\emdash\\emdash\\emdash\\emdash\\emdash}}\\par"
            );
        }
        Block::Table(table) => write_table(out, table, indent, width),
        Block::Image { url, alt } => {
            let label = if alt.is_empty() { url } else { alt };
            let _ = write!(out, "\\pard\\li{indent}\\sa120{{\\cf3 [image: ");
            push_text(out, label);
            out.push_str("]}\\par\n");
        }
    }
}

fn write_list(out: &mut String, items: &[ListItem], start: Option<u64>, indent: u32, width: u32) {
    let inner = indent + INDENT;
    for (index, item) in items.iter().enumerate() {
        let marker = match (item.checked, start) {
            (Some(true), _) => "\\u9745?".to_owned(),
            (Some(false), _) => "\\u9744?".to_owned(),
            (None, Some(start)) => format!("{}.", start + index as u64),
            (None, None) => "\\bullet".to_owned(),
        };
        let _ = write!(
            out,
            "\\pard\\fi-280\\li{inner}\\tx{inner}\\sa60 {marker}\\tab "
        );
        write_inline(out, &item.content);
        out.push_str("\\par\n");
        write_blocks(out, &item.children, inner, width);
    }
    let _ = writeln!(out, "\\pard\\sa60\\par");
}

/// A table, its columns as wide as their longest text allows within
/// `width` twips.
fn write_table(out: &mut String, table: &TableData, indent: u32, width: u32) {
    let columns = table
        .rows
        .iter()
        .map(Vec::len)
        .chain(Some(table.header.len()))
        .max()
        .unwrap_or(0);
    if columns == 0 {
        return;
    }
    let longest: Vec<u32> = (0..columns)
        .map(|column| {
            std::iter::once(&table.header)
                .chain(&table.rows)
                .filter_map(|row| row.get(column))
                .map(|cell| cell.text.chars().count())
                .max()
                .unwrap_or(0)
                .clamp(3, 40) as u32
        })
        .collect();
    let total: u32 = longest.iter().sum();
    let room = width.saturating_sub(indent).max(1500);
    let border = "\\brdrs\\brdrw10\\brdrcf5";
    for (index, row) in std::iter::once(&table.header)
        .chain(&table.rows)
        .enumerate()
    {
        let header = index == 0;
        let _ = write!(out, "\\trowd\\trgaph100\\trleft{indent}");
        let mut right = indent;
        for share in &longest {
            right += room * share / total;
            let _ = write!(
                out,
                "\\clbrdrt{border}\\clbrdrl{border}\\clbrdrb{border}\\clbrdrr{border}"
            );
            if header {
                out.push_str("\\clcbpat4");
            }
            let _ = write!(out, "\\cellx{right}");
        }
        out.push_str("\\pard\\intbl ");
        for column in 0..columns {
            let align = match table.alignments.get(column) {
                Some(Alignment::Center) => "\\qc",
                Some(Alignment::Right) => "\\qr",
                _ => "\\ql",
            };
            out.push_str(align);
            out.push_str(if header { "{\\b " } else { "{" });
            if let Some(cell) = row.get(column) {
                write_inline(out, cell);
            }
            out.push_str("}\\cell ");
        }
        out.push_str("\\row\n");
    }
    let _ = writeln!(out, "\\pard\\sa120\\par");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_rich_text() {
        let rtf = to_rtf(
            "# Title\n\nSome **bold**, *it*, `code`, {braces} and café.\n\n\
             - one\n- [x] done\n\n2. two\n\n> quoted\n\n```\nfn x() {}\n```\n\n\
             | A | B |\n|:-:|--:|\n| 1 | 2 |\n\n---\n\n![pic](a.png)\n",
            9000,
        );
        for expected in [
            "{\\rtf1",
            "{\\b\\fs40 Title}\\par",
            "{\\b bold}",
            "{\\i it}",
            "{\\f1\\fs19\\highlight4 code}",
            "\\{braces\\}",
            "caf\\u233?",
            "\\bullet\\tab one",
            "\\u9745?\\tab done",
            "2.\\tab two",
            "{\\cf3 \\pard",
            "fn x() \\{\\}",
            "\\trowd",
            "\\qc{\\b A}\\cell",
            "\\qr{2}\\cell",
            "\\emdash",
            "[image: pic]",
        ] {
            assert!(rtf.contains(expected), "{expected:?} missing from:\n{rtf}");
        }
        assert!(rtf.ends_with('}'));
        // Groups balance (escaped braces aside).
        let open = rtf.matches('{').count() - rtf.matches("\\{").count();
        let close = rtf.matches('}').count() - rtf.matches("\\}").count();
        assert_eq!(open, close);
    }
}
