//! Lists continue themselves: Enter on a bullet, numbered, task or quote
//! line starts the next one (`- `, `4. `, `- [ ] `, `> `), with the same
//! indent; Enter on an empty one ends the list (its marker goes).  Done by
//! handing the editor the text to type instead of the Enter, so it undoes
//! like typing.

use eframe::egui;

/// What Enter does at the caret instead of a plain line break.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OnEnter {
    /// Type this (a line break and the next item's marker).
    Type(String),
    /// The item is empty: remove its marker (the line up to the caret,
    /// this many characters), which ends the list.
    EndList(usize),
}

/// The list or quote marker at the start of `line`: its length in bytes
/// and the marker the next line gets (indent and every quote level
/// included), or `None` for other lines.
fn marker(line: &str) -> Option<(usize, String)> {
    let mut at = line.len() - line.trim_start_matches([' ', '\t']).len();
    let mut next = line[..at].to_owned();
    // Quote levels: a `>` each, with an optional space.
    let mut quoted = false;
    while line[at..].starts_with('>') {
        at += 1;
        if line[at..].starts_with(' ') {
            at += 1;
        }
        next.push_str("> ");
        quoted = true;
    }
    let rest = &line[at..];
    let list_indent = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    match list_marker(&rest[list_indent..]) {
        Some((len, list_next)) => {
            next.push_str(&rest[..list_indent]);
            next.push_str(&list_next);
            Some((at + list_indent + len, next))
        }
        None if quoted => Some((at, next)),
        None => None,
    }
}

/// A bullet, task or numbered list marker at the start of `rest`: its
/// length in bytes and the next item's marker.
fn list_marker(rest: &str) -> Option<(usize, String)> {
    let first = rest.chars().next()?;
    match first {
        '-' | '*' | '+' => {
            if !rest[1..].starts_with(' ') {
                return None;
            }
            let task = matches!(rest[2..].get(..4), Some("[ ] " | "[x] " | "[X] "));
            Some(if task {
                (6, format!("{first} [ ] "))
            } else {
                (2, format!("{first} "))
            })
        }
        '0'..='9' => {
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let delimiter = rest[digits..].chars().next()?;
            if digits > 9 || !matches!(delimiter, '.' | ')') || !rest[digits + 1..].starts_with(' ')
            {
                return None;
            }
            let number: u64 = rest[..digits].parse().ok()?;
            Some((digits + 2, format!("{}{delimiter} ", number + 1)))
        }
        _ => None,
    }
}

/// Whether the line starting at byte `line_start` of `text` is inside a
/// fenced code block (where nothing is a list).
fn in_code_block(text: &str, line_start: usize) -> bool {
    text[..line_start]
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("```") || line.starts_with("~~~")
        })
        .count()
        % 2
        == 1
}

/// What Enter should do with the caret at byte `caret` of `text`, or
/// `None` for a plain line break.
pub(crate) fn on_enter(text: &str, caret: usize) -> Option<OnEnter> {
    let caret = caret.min(text.len());
    let line_start = text[..caret].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[caret..].find('\n').map_or(text.len(), |at| caret + at);
    let line = text[line_start..line_end].trim_end_matches('\r');
    if in_code_block(text, line_start) {
        return None;
    }
    let (marker_len, next) = marker(line)?;
    let caret_in_line = caret - line_start;
    if caret_in_line < marker_len {
        // Enter inside the marker: a plain line break.
        return None;
    }
    if line[marker_len..].trim().is_empty() {
        return Some(OnEnter::EndList(text[line_start..caret].chars().count()));
    }
    Some(OnEnter::Type(format!("\n{next}")))
}

/// Before the editor reads this frame's input: when Enter (alone) is
/// pressed with the caret (no selection) at `caret_char` on a list line of
/// `text`, swap the key for the text or deletions [`on_enter`] gives.
pub(crate) fn handle_enter(ui: &egui::Ui, text: &str, caret_char: usize) {
    // Shift+Enter and the like stay a plain line break.
    if !ui.input(|input| input.modifiers.is_none()) {
        return;
    }
    let caret = crate::edit_ops::char_to_byte(text, caret_char);
    let Some(action) = on_enter(text, caret) else {
        return;
    };
    if !ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        return;
    }
    let events: Vec<egui::Event> = match action {
        OnEnter::Type(typed) => vec![egui::Event::Text(typed)],
        OnEnter::EndList(count) => (0..count)
            .map(|_| egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .collect(),
    };
    ui.input_mut(|input| input.events.extend(events));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter_at_end(line: &str) -> Option<OnEnter> {
        on_enter(line, line.len())
    }

    #[allow(clippy::unnecessary_wraps)] // Compared with `on_enter`'s result.
    fn typed(text: &str) -> Option<OnEnter> {
        Some(OnEnter::Type(text.to_owned()))
    }

    #[test]
    fn markers_continue_with_their_indent() {
        assert_eq!(enter_at_end("- milk"), typed("\n- "));
        assert_eq!(enter_at_end("* milk"), typed("\n* "));
        assert_eq!(enter_at_end("    + nested"), typed("\n    + "));
        assert_eq!(enter_at_end("3. third"), typed("\n4. "));
        assert_eq!(enter_at_end("9) ninth"), typed("\n10) "));
        assert_eq!(enter_at_end("- [x] done"), typed("\n- [ ] "));
        assert_eq!(enter_at_end("- [ ] todo"), typed("\n- [ ] "));
        assert_eq!(enter_at_end("> quoted"), typed("\n> "));
    }

    #[test]
    fn quotes_keep_every_level_and_their_lists() {
        assert_eq!(enter_at_end("> > nested"), typed("\n> > "));
        assert_eq!(enter_at_end(">> nested"), typed("\n> > "));
        assert_eq!(enter_at_end("> - item"), typed("\n> - "));
        assert_eq!(enter_at_end("> 2. item"), typed("\n> 3. "));
        assert_eq!(enter_at_end("> > "), Some(OnEnter::EndList(4)));
    }

    #[test]
    fn empty_items_end_the_list() {
        assert_eq!(enter_at_end("- "), Some(OnEnter::EndList(2)));
        assert_eq!(enter_at_end("  12. "), Some(OnEnter::EndList(6)));
        assert_eq!(enter_at_end("- [ ] "), Some(OnEnter::EndList(6)));
        assert_eq!(enter_at_end(">"), Some(OnEnter::EndList(1)));
    }

    #[test]
    fn other_lines_and_code_blocks_get_a_plain_line_break() {
        for line in ["plain", "-not a list", "1.5 litres", "#3 issue", ""] {
            assert_eq!(enter_at_end(line), None, "{line:?}");
        }
        let text = "```\n- not a list\n";
        assert_eq!(on_enter(text, text.len() - 1), None);
        let text = "```\ncode\n```\n- item";
        assert_eq!(on_enter(text, text.len()), typed("\n- "));
        // In the marker itself: a plain line break.
        assert_eq!(on_enter("- milk", 1), None);
        // In the middle of an item: the rest moves to the new item.
        assert_eq!(on_enter("- milk and eggs", 6), typed("\n- "));
        // CRLF documents.
        assert_eq!(on_enter("- a\r\n- b\r\n", 8), typed("\n- "));
    }
}
