//! Line endings.
//!
//! Like Notepad, a document keeps its file's line endings.  Every document
//! has an [`EndOfLine`] style: detected when the file is loaded (the more
//! common of CRLF and LF, a tie counting as CRLF), and
//! [`EndOfLine::NEW_DOCUMENT`] for a new Untitled document.  Line breaks
//! added while editing (typing, paste, formatting, replace) are
//! converted to that style with [`normalize`], so a CRLF document never gains
//! a lone `\n` and an LF document never gains a `\r\n`.  Only the added text
//! is converted: a file that is mixed on disk stays mixed except where it is
//! edited.
//!
//! [`LineEnding`] is what a text actually contains, for the status bar.

use std::borrow::Cow;

/// The line break a document writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndOfLine {
    Lf,
    CrLf,
}

impl EndOfLine {
    /// Style of a new Untitled document: CRLF on Windows (Notepad's
    /// default), LF on the (non-Windows) dev host.
    pub(crate) const NEW_DOCUMENT: Self = if cfg!(windows) { Self::CrLf } else { Self::Lf };

    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }

    /// The more common of CRLF and LF in `text` (a tie is CRLF); `None`
    /// when it has neither.  Lone `\r`s do not count: the editor does not
    /// break lines at them.
    #[must_use]
    pub(crate) fn detect(text: &str) -> Option<Self> {
        let counts = BreakCounts::of(text);
        match (counts.crlf, counts.lf) {
            (0, 0) => None,
            (crlf, lf) if crlf >= lf => Some(Self::CrLf),
            _ => Some(Self::Lf),
        }
    }
}

/// Line-ending style a text actually uses (status bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineEnding {
    Crlf,
    Lf,
    Cr,
    Mixed,
}

impl LineEnding {
    /// Scan `text` once; `None` when it has no line breaks.
    #[must_use]
    pub(crate) fn detect(text: &str) -> Option<Self> {
        let counts = BreakCounts::of(text);
        match (counts.crlf > 0, counts.lf > 0, counts.cr > 0) {
            (false, false, false) => None,
            (true, false, false) => Some(Self::Crlf),
            (false, true, false) => Some(Self::Lf),
            (false, false, true) => Some(Self::Cr),
            _ => Some(Self::Mixed),
        }
    }

    /// Notepad's status-bar wording.
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Crlf => "Windows (CRLF)",
            Self::Lf => "Unix (LF)",
            Self::Cr => "Macintosh (CR)",
            Self::Mixed => "Mixed",
        }
    }
}

impl From<EndOfLine> for LineEnding {
    fn from(eol: EndOfLine) -> Self {
        match eol {
            EndOfLine::Lf => Self::Lf,
            EndOfLine::CrLf => Self::Crlf,
        }
    }
}

/// Line breaks in a text, by kind.
#[derive(Default)]
struct BreakCounts {
    crlf: usize,
    lf: usize,
    cr: usize,
}

impl BreakCounts {
    fn of(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut counts = Self::default();
        let mut iter = memchr::memchr2_iter(b'\r', b'\n', bytes);
        while let Some(pos) = iter.next() {
            if bytes[pos] == b'\n' {
                counts.lf += 1;
            } else if bytes.get(pos + 1) == Some(&b'\n') {
                counts.crlf += 1;
                let _ = iter.next(); // the '\n' of this CRLF
            } else {
                counts.cr += 1;
            }
        }
        counts
    }
}

/// `text` with every line break (`\r\n`, `\n` or a lone `\r`) written as
/// `eol`.  Borrows when nothing changes.
#[must_use]
pub(crate) fn normalize(text: &str, eol: EndOfLine) -> Cow<'_, str> {
    let counts = BreakCounts::of(text);
    let foreign = match eol {
        EndOfLine::Lf => counts.crlf + counts.cr,
        EndOfLine::CrLf => counts.lf + counts.cr,
    };
    if foreign == 0 {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len() + counts.lf + counts.cr);
    let mut start = 0;
    let mut iter = memchr::memchr2_iter(b'\r', b'\n', bytes);
    while let Some(pos) = iter.next() {
        out.push_str(&text[start..pos]);
        out.push_str(eol.as_str());
        start = pos + 1;
        if bytes[pos] == b'\r' && bytes.get(pos + 1) == Some(&b'\n') {
            let _ = iter.next();
            start = pos + 2;
        }
    }
    out.push_str(&text[start..]);
    Cow::Owned(out)
}

/// Whether byte offset `pos` falls between the `\r` and `\n` of a pair.
#[must_use]
pub(crate) fn inside_crlf(text: &str, pos: usize) -> bool {
    let bytes = text.as_bytes();
    pos > 0 && bytes.get(pos) == Some(&b'\n') && bytes[pos - 1] == b'\r'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ending_detection() {
        let cases = [
            ("", None),
            ("no breaks", None),
            ("a\r\nb\r\n", Some(LineEnding::Crlf)),
            ("a\nb\n", Some(LineEnding::Lf)),
            ("a\rb\r", Some(LineEnding::Cr)),
            ("a\r\nb\n", Some(LineEnding::Mixed)),
            ("a\rb\n", Some(LineEnding::Mixed)),
            ("a\r\nb\rc", Some(LineEnding::Mixed)),
            ("\r", Some(LineEnding::Cr)),
            ("\r\n", Some(LineEnding::Crlf)),
            ("\n\r", Some(LineEnding::Mixed)),
            ("é\r\n日本\r\n", Some(LineEnding::Crlf)),
        ];
        for (text, expected) in cases {
            assert_eq!(LineEnding::detect(text), expected, "{text:?}");
        }
        assert_eq!(LineEnding::Crlf.label(), "Windows (CRLF)");
        assert_eq!(LineEnding::Lf.label(), "Unix (LF)");
        assert_eq!(LineEnding::Cr.label(), "Macintosh (CR)");
        assert_eq!(LineEnding::Mixed.label(), "Mixed");
        assert_eq!(LineEnding::from(EndOfLine::CrLf), LineEnding::Crlf);
        assert_eq!(LineEnding::from(EndOfLine::Lf), LineEnding::Lf);
    }

    #[test]
    fn document_style_is_the_dominant_one_ties_are_crlf() {
        use EndOfLine::{CrLf, Lf};
        let cases = [
            ("", None),
            ("one line", None),
            ("a\rb\r", None),
            ("a\r\nb\r\n", Some(CrLf)),
            ("a\nb\n", Some(Lf)),
            ("a\r\nb\nc\r\n", Some(CrLf)),
            ("a\nb\r\nc\n", Some(Lf)),
            ("a\r\nb\n", Some(CrLf)),
            ("a\nb\r\n", Some(CrLf)),
            ("a\nb\rc\rd\r", Some(Lf)),
        ];
        for (text, expected) in cases {
            assert_eq!(EndOfLine::detect(text), expected, "{text:?}");
        }
        assert_eq!(CrLf.as_str(), "\r\n");
        assert_eq!(Lf.as_str(), "\n");
        let expected = if cfg!(windows) { CrLf } else { Lf };
        assert_eq!(EndOfLine::NEW_DOCUMENT, expected);
    }

    #[test]
    fn normalize_converts_every_break_and_borrows_when_done() {
        use EndOfLine::{CrLf, Lf};
        let cases = [
            ("a\nb", CrLf, "a\r\nb"),
            ("a\r\nb\nc\rd", CrLf, "a\r\nb\r\nc\r\nd"),
            ("\n\n", CrLf, "\r\n\r\n"),
            ("\r\r\n", CrLf, "\r\n\r\n"),
            ("a\r\nb\nc\rd", Lf, "a\nb\nc\nd"),
            ("\r\n\r\n", Lf, "\n\n"),
            ("é\n🦀\r\n", CrLf, "é\r\n🦀\r\n"),
        ];
        for (text, eol, expected) in cases {
            let out = normalize(text, eol);
            assert_eq!(out, expected, "{text:?} as {eol:?}");
            assert!(matches!(out, Cow::Owned(_)), "{text:?}");
        }
        for (text, eol) in [
            ("", CrLf),
            ("plain", Lf),
            ("a\r\nb\r\n", CrLf),
            ("a\nb\n", Lf),
        ] {
            assert!(
                matches!(normalize(text, eol), Cow::Borrowed(s) if s == text),
                "{text:?}"
            );
        }
    }

    #[test]
    fn inside_crlf_only_between_the_pair() {
        let text = "a\r\nb\nc\r";
        let inside: Vec<usize> = (0..=text.len())
            .filter(|&pos| inside_crlf(text, pos))
            .collect();
        assert_eq!(inside, [2]);
        assert!(!inside_crlf("", 0));
        assert!(!inside_crlf("\n", 0));
    }
}

/// The rules above end to end: keys and pastes into a real `TextEdit`,
/// undo, formatting, replace, load / save and disk reloads.
#[cfg(test)]
mod editing_tests {
    use std::{fs, path::PathBuf, sync::Arc, time::SystemTime};

    use eframe::egui::{self, Key, Modifiers};

    use super::{EndOfLine, LineEnding};
    use crate::{
        AppCommand, RustdownApp, disk::io::DiskRevision, md_commands::MdCommand, tab::Tab,
    };

    const CRLF: EndOfLine = EndOfLine::CrLf;
    const LF: EndOfLine = EndOfLine::Lf;

    fn ctx_with_fonts() -> egui::Context {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx
    }

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn press(key_: Key) -> Vec<egui::Event> {
        vec![key(key_, Modifiers::NONE)]
    }

    fn typed(text: &str) -> Vec<egui::Event> {
        vec![egui::Event::Text(text.to_owned())]
    }

    /// One frame of the active editor with `events` as its input.
    fn frame(ctx: &egui::Context, app: &mut RustdownApp, events: Vec<egui::Event>) {
        let mut input = crate::diagnostics::diagnostics_raw_input();
        input.events = events;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
        });
    }

    /// Show the active editor, put the caret at char `caret` and focus it.
    fn focus_at(ctx: &egui::Context, app: &mut RustdownApp, caret: usize) {
        frame(ctx, app, Vec::new());
        app.select_in_editor(ctx, caret..caret);
        frame(ctx, app, Vec::new());
        assert!(ctx.memory(|m| m.has_focus(app.tab().editor_id())));
    }

    fn editor_with(ctx: &egui::Context, text: &str, eol: EndOfLine, caret: usize) -> RustdownApp {
        let mut app = RustdownApp::default();
        app.tab_mut().doc.text = Arc::new(text.to_owned());
        app.tab_mut().doc.line_ending = eol;
        focus_at(ctx, &mut app, caret);
        app
    }

    fn text(app: &RustdownApp) -> &str {
        app.tab().doc.text.as_str()
    }

    fn status_line_ending(ctx: &egui::Context, app: &mut RustdownApp) -> LineEnding {
        app.tab_mut().refresh_stats_now();
        app.status_info(ctx).line_ending
    }

    /// `true` when every `\n` in `text` belongs to a `\r\n`.
    fn only_crlf(text: &str) -> bool {
        !text.replace("\r\n", "").contains(['\r', '\n'])
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustdown-eol-{name}-{}", std::process::id()));
        assert!(fs::create_dir_all(&dir).is_ok());
        dir
    }

    fn read(path: &std::path::Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|err| unreachable!("{err}"))
    }

    fn rev(seconds: u64) -> DiskRevision {
        DiskRevision {
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
            len: seconds,
            #[cfg(unix)]
            dev: 0,
            #[cfg(unix)]
            inode: 0,
        }
    }

    #[test]
    fn enter_writes_the_documents_line_break_and_moves_the_caret_past_it() {
        let ctx = ctx_with_fonts();
        let mut app = editor_with(&ctx, "ab\r\ncd", CRLF, 1);
        frame(&ctx, &mut app, press(Key::Enter));
        assert_eq!(text(&app), "a\r\nb\r\ncd");
        assert_eq!(app.editor_selection(&ctx), 3..3, "after the \\n");
        frame(&ctx, &mut app, typed("x"));
        assert_eq!(text(&app), "a\r\nxb\r\ncd");
        assert!(app.tab().doc.dirty);
        assert_eq!(status_line_ending(&ctx, &mut app), LineEnding::Crlf);

        let ctx = ctx_with_fonts();
        let mut app = editor_with(&ctx, "ab\ncd", LF, 1);
        frame(&ctx, &mut app, press(Key::Enter));
        assert_eq!(text(&app), "a\nb\ncd");
        assert_eq!(app.editor_selection(&ctx), 2..2);
        assert_eq!(status_line_ending(&ctx, &mut app), LineEnding::Lf);
    }

    #[test]
    fn pasted_line_breaks_take_the_documents_style() {
        let ctx = ctx_with_fonts();
        let mut app = editor_with(&ctx, "ab\r\n", CRLF, 1);
        let paste = vec![egui::Event::Paste("1\n2\r\n3\r4\n".to_owned())];
        frame(&ctx, &mut app, paste);
        assert_eq!(text(&app), "a1\r\n2\r\n3\r\n4\r\nb\r\n");
        assert_eq!(app.editor_selection(&ctx), 13..13, "caret after the paste");

        let ctx = ctx_with_fonts();
        let mut app = editor_with(&ctx, "ab\n", LF, 1);
        frame(
            &ctx,
            &mut app,
            vec![egui::Event::Paste("1\r\n2\r\n".to_owned())],
        );
        assert_eq!(text(&app), "a1\n2\nb\n");
        assert_eq!(app.editor_selection(&ctx), 5..5);
        assert_eq!(status_line_ending(&ctx, &mut app), LineEnding::Lf);
    }

    #[test]
    fn backspace_and_delete_remove_a_crlf_as_one_line_break() {
        let ctx = ctx_with_fonts();
        let mut app = editor_with(&ctx, "ab\r\ncd", CRLF, 4);
        frame(&ctx, &mut app, press(Key::Backspace));
        assert_eq!(text(&app), "abcd");
        assert_eq!(app.editor_selection(&ctx), 2..2);
        frame(&ctx, &mut app, press(Key::Enter));
        assert_eq!(text(&app), "ab\r\ncd");
        app.select_in_editor(&ctx, 2..2);
        frame(&ctx, &mut app, press(Key::Delete));
        assert_eq!(text(&app), "abcd");
        assert_eq!(app.editor_selection(&ctx), 2..2);
    }

    #[test]
    fn caret_never_rests_between_cr_and_lf() {
        let ctx = ctx_with_fonts();
        let mut app = editor_with(&ctx, "ab\r\ncd", CRLF, 4);
        // One arrow press crosses the line break, both ways.
        frame(&ctx, &mut app, press(Key::ArrowLeft));
        assert_eq!(app.editor_selection(&ctx), 2..2);
        frame(&ctx, &mut app, press(Key::ArrowRight));
        assert_eq!(app.editor_selection(&ctx), 4..4);
        // End stops before the invisible '\r': Backspace then deletes 'b'.
        app.select_in_editor(&ctx, 0..0);
        frame(&ctx, &mut app, press(Key::End));
        assert_eq!(app.editor_selection(&ctx), 2..2);
        frame(&ctx, &mut app, press(Key::Backspace));
        assert_eq!(text(&app), "a\r\ncd");
        // Shift+Right at the end of a line selects the whole line break.
        frame(&ctx, &mut app, vec![key(Key::ArrowRight, Modifiers::SHIFT)]);
        assert_eq!(app.editor_selection(&ctx), 1..3);
        frame(&ctx, &mut app, typed("x"));
        assert_eq!(text(&app), "axcd");
    }

    #[test]
    fn undo_and_redo_never_bring_back_a_lone_lf() {
        let ctx = ctx_with_fonts();
        let mut app = editor_with(&ctx, "ab\r\ncd", CRLF, 1);
        frame(&ctx, &mut app, press(Key::Enter));
        assert_eq!(text(&app), "a\r\nb\r\ncd");
        frame(&ctx, &mut app, vec![key(Key::Z, Modifiers::COMMAND)]);
        assert_eq!(text(&app), "ab\r\ncd");
        assert_eq!(app.editor_selection(&ctx), 1..1);
        frame(&ctx, &mut app, vec![key(Key::Y, Modifiers::COMMAND)]);
        assert_eq!(text(&app), "a\r\nb\r\ncd");

        // Formatting pushes its own undo points; a document with no line
        // break yet still gets its style.
        let ctx = ctx_with_fonts();
        let mut app = editor_with(&ctx, "x", CRLF, 1);
        app.execute_format(MdCommand::CodeBlock, &ctx);
        frame(&ctx, &mut app, Vec::new());
        let formatted = "x\r\n\r\n```\r\n\r\n```\r\n";
        assert_eq!(text(&app), formatted);
        frame(&ctx, &mut app, press(Key::Enter));
        assert_eq!(text(&app), "x\r\n\r\n```\r\n\r\n\r\n```\r\n");
        for (keys, expected) in [
            (key(Key::Z, Modifiers::COMMAND), formatted),
            (key(Key::Z, Modifiers::COMMAND), "x"),
            (key(Key::Y, Modifiers::COMMAND), formatted),
        ] {
            frame(&ctx, &mut app, vec![keys]);
            assert_eq!(text(&app), expected);
        }
        assert!(only_crlf(text(&app)));
    }

    #[test]
    fn new_untitled_documents_use_the_injected_default() {
        let ctx = ctx_with_fonts();
        let mut app = RustdownApp::default();
        assert_eq!(app.tab().doc.line_ending, EndOfLine::NEW_DOCUMENT);
        app.untitled_line_ending = CRLF; // Windows
        app.execute(AppCommand::NewTab, &ctx);
        assert_eq!(app.tab().doc.line_ending, CRLF);
        assert_eq!(
            status_line_ending(&ctx, &mut app),
            LineEnding::Crlf,
            "no line break yet: shows what Enter inserts"
        );
        focus_at(&ctx, &mut app, 0);
        for events in [typed("a"), press(Key::Enter), typed("b")] {
            frame(&ctx, &mut app, events);
        }
        assert_eq!(text(&app), "a\r\nb");

        app.untitled_line_ending = LF;
        app.execute(AppCommand::NewTab, &ctx);
        assert_eq!(app.tab().doc.line_ending, LF);
        assert_eq!(status_line_ending(&ctx, &mut app), LineEnding::Lf);
    }

    #[test]
    fn crlf_file_keeps_crlf_through_edit_and_save() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("notes.md");
        assert!(fs::write(&path, "# Notes\r\n\r\nline\r\n").is_ok());
        let Ok(tab) = Tab::from_path(path.clone()) else {
            unreachable!()
        };
        assert_eq!(tab.doc.line_ending, CRLF);
        let mut app = RustdownApp {
            tabs: vec![tab],
            untitled_line_ending: LF,
            ..RustdownApp::default()
        };
        let ctx = ctx_with_fonts();
        focus_at(&ctx, &mut app, 15); // end of "line"
        for events in [press(Key::Enter), typed("more")] {
            frame(&ctx, &mut app, events);
        }
        assert!(app.tab_mut().save_doc(false));
        assert_eq!(read(&path), "# Notes\r\n\r\nline\r\nmore\r\n");
        assert_eq!(status_line_ending(&ctx, &mut app), LineEnding::Crlf);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mixed_file_is_left_alone_until_edited_then_only_the_edit_converts() {
        let dir = temp_dir("mixed");
        let path = dir.join("mixed.md");
        let on_disk = "a\r\nb\nc\r\nd";
        assert!(fs::write(&path, on_disk).is_ok());
        let Ok(tab) = Tab::from_path(path.clone()) else {
            unreachable!()
        };
        assert_eq!(tab.doc.text.as_str(), on_disk, "loaded as is");
        assert_eq!(tab.doc.line_ending, CRLF, "the dominant style");
        let mut app = RustdownApp {
            tabs: vec![tab],
            ..RustdownApp::default()
        };
        let ctx = ctx_with_fonts();
        assert_eq!(status_line_ending(&ctx, &mut app), LineEnding::Mixed);
        assert!(app.tab_mut().save_doc(false));
        assert_eq!(read(&path), on_disk, "saved as is");

        focus_at(&ctx, &mut app, 4); // after "b", before its lone '\n'
        frame(&ctx, &mut app, press(Key::Enter));
        assert!(app.tab_mut().save_doc(false));
        assert_eq!(read(&path), "a\r\nb\r\n\nc\r\nd", "other lines untouched");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_all_and_format_document_keep_the_documents_style() {
        let mut tab = Tab::default();
        tab.init_document(None, "x y\r\n".to_owned(), None);
        assert_eq!(tab.replace_all_matches(" ", "\n"), 1);
        assert_eq!(tab.doc.text.as_str(), "x\r\ny\r\n");

        // One stray CRLF no longer turns the whole LF document into CRLF.
        tab.init_document(None, "a \nb\nc\r\n".to_owned(), None);
        assert_eq!(tab.doc.line_ending, LF);
        tab.format_document();
        assert_eq!(tab.doc.text.as_str(), "a\nb\nc\n");

        // `.editorconfig`'s `end_of_line` wins and becomes the style.
        let dir = temp_dir("editorconfig");
        assert!(
            fs::write(
                dir.join(".editorconfig"),
                "root = true\n[*]\nend_of_line = lf\n"
            )
            .is_ok()
        );
        tab.init_document(Some(dir.join("x.md")), "a\r\nb\r\n".to_owned(), None);
        assert_eq!(tab.doc.line_ending, CRLF);
        tab.format_document();
        assert_eq!(tab.doc.text.as_str(), "a\nb\n");
        assert_eq!(tab.doc.line_ending, LF);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clean_reload_takes_the_files_line_endings() {
        let mut tab = Tab::default();
        // A clean reload takes the file's line endings.
        tab.load_document(PathBuf::from("m.md"), "a\r\n".to_owned(), Some(rev(1)));
        tab.incorporate_disk_text("x\ny\n".to_owned(), rev(2));
        assert_eq!(tab.doc.text.as_str(), "x\ny\n");
        assert_eq!(tab.doc.line_ending, LF);
    }
}
