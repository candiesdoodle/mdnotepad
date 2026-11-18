#![forbid(unsafe_code)]
//! Edit → Copy as formatted text: Markdown as HTML for the clipboard, so
//! pasting into an email or Word keeps headings, bold, lists, tables and
//! links.  Built from the same parse as the preview.

use std::fmt::Write as _;

use super::parse::{Alignment, Block, ListItem, StyledText, parse_markdown_into};

/// Heading font sizes, H1 to H6 (as browsers draw them).
const HEADING_SIZES: [&str; 6] = ["2em", "1.5em", "1.17em", "1em", "0.83em", "0.67em"];

/// Table cells' look when pasted (email and Word keep inline styles).
const CELL_STYLE: &str = "border:1px solid #c8c8c8;padding:4px 8px";

/// `markdown` as an HTML fragment (no `<html>` or `<body>`).
#[must_use]
pub(crate) fn to_html(markdown: &str) -> String {
    let mut blocks = Vec::new();
    parse_markdown_into(markdown, &mut blocks);
    let mut out = String::new();
    write_blocks(&mut out, &blocks);
    out
}

/// `fragment` in the Windows clipboard's "HTML Format": a header giving
/// the byte offsets of the HTML and of the fragment, then the document.
#[must_use]
pub(crate) fn clipboard_html(fragment: &str) -> String {
    const PREFIX: &str = "<html><body>\r\n<!--StartFragment-->";
    const SUFFIX: &str = "<!--EndFragment-->\r\n</body></html>";
    let header = |start_html: usize, end_html: usize, start: usize, end: usize| {
        format!(
            "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\n\
             StartFragment:{start:010}\r\nEndFragment:{end:010}\r\n"
        )
    };
    // The header has the same length whatever the (10-digit) numbers.
    let start_html = header(0, 0, 0, 0).len();
    let start = start_html + PREFIX.len();
    let end = start + fragment.len();
    let end_html = end + SUFFIX.len();
    format!(
        "{}{PREFIX}{fragment}{SUFFIX}",
        header(start_html, end_html, start, end)
    )
}

fn write_blocks(out: &mut String, blocks: &[Block]) {
    for block in blocks {
        write_block(out, block);
    }
}

fn write_block(out: &mut String, block: &Block) {
    match block {
        Block::Heading { level, text } => {
            let level = (*level).clamp(1, 6);
            // Sizes and bold spelled out: they survive pasting with
            // Word's "Merge formatting", which drops heading styles.
            let size = HEADING_SIZES[usize::from(level - 1)];
            let _ = write!(
                out,
                "<h{level} style=\"font-size:{size};font-weight:bold\">"
            );
            write_inline(out, text);
            let _ = writeln!(out, "</h{level}>");
        }
        Block::Paragraph(text) => {
            out.push_str("<p>");
            write_inline(out, text);
            out.push_str("</p>\n");
        }
        // Mermaid diagrams: drawn by the page's browser (`print`).
        Block::Code { language, code } if language.eq_ignore_ascii_case("mermaid") => {
            out.push_str("<pre class=\"mermaid\">");
            push_escaped(out, code.trim_end_matches('\n'));
            out.push_str("</pre>\n");
        }
        Block::Code { code, .. } => {
            out.push_str("<pre><code>");
            push_escaped(out, code.trim_end_matches('\n'));
            out.push_str("</code></pre>\n");
        }
        Block::Quote(inner) => {
            out.push_str("<blockquote>\n");
            write_blocks(out, inner);
            out.push_str("</blockquote>\n");
        }
        Block::UnorderedList(items) => {
            out.push_str("<ul>\n");
            write_items(out, items);
            out.push_str("</ul>\n");
        }
        Block::OrderedList { start, items } => {
            if *start == 1 {
                out.push_str("<ol>\n");
            } else {
                let _ = writeln!(out, "<ol start=\"{start}\">");
            }
            write_items(out, items);
            out.push_str("</ol>\n");
        }
        Block::ThematicBreak => out.push_str("<hr>\n"),
        Block::Table(table) => {
            out.push_str("<table style=\"border-collapse:collapse\">\n<thead><tr>");
            for (index, cell) in table.header.iter().enumerate() {
                write_cell(out, "th", cell, table.alignments.get(index).copied());
            }
            out.push_str("</tr></thead>\n<tbody>\n");
            for row in &table.rows {
                out.push_str("<tr>");
                for (index, cell) in row.iter().enumerate() {
                    write_cell(out, "td", cell, table.alignments.get(index).copied());
                }
                out.push_str("</tr>\n");
            }
            out.push_str("</tbody>\n</table>\n");
        }
        Block::Image { url, alt } => {
            out.push_str("<p><img src=\"");
            push_escaped(out, url);
            out.push_str("\" alt=\"");
            push_escaped(out, alt);
            out.push_str("\"></p>\n");
        }
    }
}

fn write_items(out: &mut String, items: &[ListItem]) {
    for item in items {
        out.push_str("<li>");
        match item.checked {
            Some(true) => out.push_str("&#9745; "),
            Some(false) => out.push_str("&#9744; "),
            None => {}
        }
        write_inline(out, &item.content);
        if !item.children.is_empty() {
            out.push('\n');
            write_blocks(out, &item.children);
        }
        out.push_str("</li>\n");
    }
}

fn write_cell(out: &mut String, tag: &str, cell: &StyledText, align: Option<Alignment>) {
    let align = match align {
        Some(Alignment::Center) => ";text-align:center",
        Some(Alignment::Right) => ";text-align:right",
        Some(Alignment::Left) => ";text-align:left",
        Some(Alignment::None) | None => "",
    };
    let _ = write!(out, "<{tag} style=\"{CELL_STYLE}{align}\">");
    write_inline(out, cell);
    let _ = write!(out, "</{tag}>");
}

/// `text` with its inline styles as tags; text outside every span is
/// written plain.
fn write_inline(out: &mut String, text: &StyledText) {
    let mut at = 0;
    for span in &text.spans {
        let start = (span.start as usize).min(text.text.len());
        let end = (span.end as usize).min(text.text.len());
        let (Some(gap), Some(piece)) = (text.text.get(at..start), text.text.get(start..end)) else {
            continue;
        };
        push_escaped(out, gap);
        let style = span.style;
        let link = text.link_url(style.link_idx);
        if let Some(url) = link {
            out.push_str("<a href=\"");
            push_escaped(out, url);
            out.push_str("\">");
        }
        let tags = [
            (style.strong(), "strong"),
            (style.emphasis(), "em"),
            (style.strikethrough(), "del"),
            (style.code(), "code"),
        ];
        for &(on, tag) in &tags {
            if on {
                let _ = write!(out, "<{tag}>");
            }
        }
        push_escaped(out, piece);
        for &(on, tag) in tags.iter().rev() {
            if on {
                let _ = write!(out, "</{tag}>");
            }
        }
        if link.is_some() {
            out.push_str("</a>");
        }
        at = end;
    }
    push_escaped(out, text.text.get(at..).unwrap_or_default());
}

/// `text` with the characters that mean something in HTML escaped.
fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_html_with_its_formatting() {
        let html = to_html(
            "# Title\n\nSome **bold**, *it*, `code` and a [link](https://x.y/?a=1&b=2).\n\n\
             - one\n- [x] done\n\n1. first\n\n> quoted\n\n```\nfn <x>\n```\n\n\
             | A | B |\n|:-|-:|\n| 1 | 2 |\n",
        );
        for expected in [
            "<h1 style=\"font-size:2em;font-weight:bold\">Title</h1>",
            "<strong>bold</strong>",
            "<em>it</em>",
            "<code>code</code>",
            "<a href=\"https://x.y/?a=1&amp;b=2\">link</a>",
            "<ul>\n<li>one</li>",
            "<li>&#9745; done</li>",
            "<ol>\n<li>first</li>",
            "<blockquote>\n<p>quoted</p>",
            "<pre><code>fn &lt;x&gt;</code></pre>",
            "<th style=\"border:1px solid #c8c8c8;padding:4px 8px;text-align:left\">A</th>",
            "text-align:right\">2</td>",
        ] {
            assert!(
                html.contains(expected),
                "{expected:?} missing from:\n{html}"
            );
        }
    }

    #[test]
    fn clipboard_header_offsets_point_at_the_html_and_the_fragment() {
        let fragment = "<p>é</p>";
        let data = clipboard_html(fragment);
        let number = |name: &str| -> usize {
            let at = data.find(name).unwrap_or_default() + name.len() + 1;
            data[at..at + 10].parse().unwrap_or_default()
        };
        assert_eq!(
            &data[number("StartFragment")..number("EndFragment")],
            fragment
        );
        assert!(data[number("StartHTML")..].starts_with("<html>"));
        assert_eq!(number("EndHTML"), data.len());
    }
}
