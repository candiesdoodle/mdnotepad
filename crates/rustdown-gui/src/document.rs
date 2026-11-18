use std::{
    borrow::Cow,
    cell::Cell,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::md::MarkdownCache;
use eframe::egui::{self, text::CCursor};

use crate::{
    disk::io::DiskRevision,
    line_ending::{self, EndOfLine},
    live_format,
};

pub struct Document {
    pub path: Option<PathBuf>,
    pub image_uri_scheme: String,
    pub text: Arc<String>,
    /// Style of the line breaks added while editing (see [`line_ending`]).
    pub line_ending: EndOfLine,
    pub base_text: Arc<String>,
    pub disk_rev: Option<DiskRevision>,
    pub stats: DocumentStats,
    pub stats_dirty: bool,
    pub preview_dirty: bool,
    pub dirty: bool,
    pub preview_cache: MarkdownCache,
    pub last_edit_at: Option<Instant>,
    pub edit_seq: u64,
    pub editor_galley_cache: Option<EditorGalleyCache>,
    /// Formatted-view analysis of `text` at `edit_seq` (see
    /// [`Document::formatted_analysis`]).
    pub formatted_analysis: Option<(u64, live_format::Analysis)>,
}

impl Default for Document {
    fn default() -> Self {
        let text = Arc::new(String::new());
        Self {
            path: None,
            image_uri_scheme: crate::default_image_uri_scheme(None),
            text: text.clone(),
            line_ending: EndOfLine::NEW_DOCUMENT,
            base_text: text,
            disk_rev: None,
            stats: DocumentStats::default(),
            stats_dirty: false,
            preview_dirty: false,
            dirty: false,
            preview_cache: MarkdownCache::default(),
            last_edit_at: None,
            edit_seq: 0,
            editor_galley_cache: None,
            formatted_analysis: None,
        }
    }
}

impl Document {
    #[must_use]
    pub fn debounce_remaining(&self, debounce: Duration) -> Option<Duration> {
        let last = self.last_edit_at?;
        let since = last.elapsed();
        debounce.checked_sub(since)
    }

    #[must_use]
    pub fn title(&self) -> Cow<'_, str> {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(|| Cow::Borrowed("Untitled"), |name| name.to_string_lossy())
    }

    #[must_use]
    pub fn path_label(&self) -> Cow<'_, str> {
        self.path
            .as_ref()
            .map_or_else(|| Cow::Borrowed("Unsaved"), |path| path.to_string_lossy())
    }

    /// Mark the document as having been edited — sets dirty flags and
    /// records the edit timestamp.
    pub fn mark_text_changed(&mut self) {
        self.dirty = true;
        self.stats_dirty = true;
        self.preview_dirty = true;
        self.last_edit_at = Some(Instant::now());
    }

    /// The formatted-view analysis of the current text (rebuilt after an
    /// edit).
    pub fn formatted_analysis(&mut self) -> &live_format::Analysis {
        formatted_analysis(&mut self.formatted_analysis, self.edit_seq, &self.text)
    }

    /// Drop the editor's layout caches (Preview mode, view or theme change).
    pub fn clear_editor_caches(&mut self) {
        self.editor_galley_cache = None;
        self.formatted_analysis = None;
    }

    /// Increment `edit_seq` monotonically (wraps at `u64::MAX`).
    pub const fn bump_edit_seq(&mut self) {
        self.edit_seq = self.edit_seq.wrapping_add(1);
    }

    /// Recompute stats from the current text if `stats_dirty` is set.
    pub fn refresh_stats_if_dirty(&mut self) {
        if self.stats_dirty {
            self.stats = DocumentStats::from_text(self.text.as_str());
            self.stats_dirty = false;
        }
    }

    /// If `preview_dirty` is set, clear the preview cache and the flag.
    /// Returns `true` if the cache was cleared.
    pub fn consume_preview_dirty(&mut self) -> bool {
        if self.preview_dirty {
            self.preview_cache.clear();
            self.preview_dirty = false;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentStats {
    pub lines: usize,
    pub words: usize,
}

impl DocumentStats {
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let lines = if text.is_empty() {
            1
        } else {
            1 + crate::md::bytecount_newlines(text.as_bytes())
        };
        let words = text.split_whitespace().count();
        Self { lines, words }
    }
}

impl Default for DocumentStats {
    fn default() -> Self {
        Self { lines: 1, words: 0 }
    }
}

/// The analysis in `slot` for `text` at edit `seq`, rebuilt when stale.
pub fn formatted_analysis<'a>(
    slot: &'a mut Option<(u64, live_format::Analysis)>,
    seq: u64,
    text: &str,
) -> &'a live_format::Analysis {
    let stale =
        !matches!(slot, Some((cached, analysis)) if *cached == seq && analysis.len() == text.len());
    if stale {
        *slot = None;
    }
    &slot
        .get_or_insert_with(|| (seq, live_format::analyze(text)))
        .1
}

/// How the editor lays the text out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorView {
    /// Plain text: one monospace font, one size, one colour (Raw and
    /// Split, and Edit for documents too large for the formatted view).
    Raw,
    /// Formatted, marks shown only on the revealed lines.
    Formatted(live_format::Reveal),
}

#[derive(Clone)]
pub struct EditorGalleyCache {
    pub content_seq: u64,
    pub content_color_mode: bool,
    pub view: EditorView,
    pub wrap_width_bits: u32,
    /// The document's text sizes the sections were built for.
    pub text_sizes_bits: [u32; 3],
    pub pixels_per_point_bits: u32,
    /// Cached layout sections (byte ranges + format) — avoids storing a full
    /// copy of the document text. The text is rebuilt from `Document.text` on
    /// partial cache hits, saving ~1× document size in steady-state memory.
    pub layout_sections: Vec<egui::text::LayoutSection>,
    pub galley: Arc<egui::Galley>,
    pub row_byte_offsets: Vec<(f32, u32)>,
    /// Formatted view: its style and the overlays placed on `galley`.
    pub format_style: Option<live_format::FormatStyle>,
    pub overlays: Vec<live_format::Overlay>,
    /// `mermaid::version()` when built: a diagram finished since means
    /// laying out again (its picture takes the place of its code).
    pub diagrams_version: u64,
    /// Diagram pictures: where they are painted on `galley`, and their URIs.
    pub pictures: Vec<(egui::Rect, String)>,
}

/// The editor's view of [`Document::text`].  Counts edits in `seq`, and
/// keeps the document's line endings: line breaks typed or pasted are
/// written as `line_ending`, and a `\r\n` pair is never split.
pub struct TrackedTextBuffer<'a, 'b> {
    pub text: &'a mut Arc<String>,
    pub seq: &'b Cell<u64>,
    pub line_ending: EndOfLine,
}

impl TrackedTextBuffer<'_, '_> {
    fn bump_seq(&self) {
        self.seq.set(self.seq.get().wrapping_add(1));
    }

    /// `char_index`, moved out of a `\r\n` pair it falls inside: before the
    /// `\r`, or after the `\n` when `forward`.
    fn out_of_crlf(&self, char_index: usize, forward: bool) -> usize {
        let byte = egui::TextBuffer::byte_index_from_char_index(self, char_index);
        if !line_ending::inside_crlf(self.text.as_str(), byte) {
            char_index
        } else if forward {
            char_index + 1
        } else {
            char_index - 1
        }
    }
}

impl egui::TextBuffer for TrackedTextBuffer<'_, '_> {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.text.as_str()
    }

    /// Enter inserts `\n` and pasted text brings its own line breaks: both
    /// are written in the document's style.  Returns the chars inserted,
    /// which is how far egui then moves the caret.
    fn insert_text(&mut self, text: &str, char_index: usize) -> usize {
        let text = line_ending::normalize(text, self.line_ending);
        let inserted = egui::TextBuffer::insert_text(Arc::make_mut(self.text), &text, char_index);
        if inserted != 0 {
            self.bump_seq();
        }
        inserted
    }

    fn delete_char_range(&mut self, char_range: std::ops::Range<usize>) {
        if char_range.start < char_range.end {
            self.bump_seq();
        }
        egui::TextBuffer::delete_char_range(Arc::make_mut(self.text), char_range);
    }

    /// egui deletes through here (Backspace, Delete, Cut, word deletes) and
    /// also before every insertion (to replace the selection), so this keeps
    /// `\r\n` pairs whole: a range ending inside a pair grows to the whole
    /// line break (Backspace at the start of a line, Delete at the end of
    /// one), and an insertion point inside a pair moves before the `\r`.
    fn delete_selected_ccursor_range(&mut self, [min, max]: [CCursor; 2]) -> CCursor {
        let start = self.out_of_crlf(min.index, false);
        let end = if max.index == min.index {
            start
        } else {
            self.out_of_crlf(max.index, true)
        };
        egui::TextBuffer::delete_char_range(self, start..end);
        CCursor {
            index: start,
            prefer_next_row: true,
        }
    }

    /// Undo / redo put back an earlier state of the document unchanged (the
    /// default clears and re-inserts, which would rewrite the line endings
    /// of a mixed file).
    fn replace_with(&mut self, text: &str) {
        *self.text = Arc::new(text.to_owned());
        self.bump_seq();
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<TrackedTextBuffer<'static, 'static>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::TextBuffer as _;

    // ── DocumentStats::from_text ──────────────────────────────────────

    #[test]
    fn stats_line_counting_cases() {
        let cases = [
            // (input, expected_lines, description)
            ("", 1, "empty string"),
            ("hello", 1, "no newline"),
            ("a\nb", 2, "single newline"),
            ("a\n", 2, "trailing newline"),
            ("a\nb\nc\n", 4, "multiple newlines"),
            ("a\r\nb\r\n", 3, "CRLF pairs"),
            ("a\rb", 1, "bare CR not counted"),
            ("a\nb\r\nc\rd", 3, "mixed newlines"),
            ("你好\n世界", 2, "CJK text"),
            ("\n\n\n", 4, "only newlines"),
        ];
        for (input, expected, desc) in cases {
            assert_eq!(
                DocumentStats::from_text(input).lines,
                expected,
                "{desc}: {input:?}"
            );
        }
        assert_eq!(DocumentStats::default(), DocumentStats::from_text(""));
    }

    // ── bytecount_newlines ────────────────────────────────────────────

    #[test]
    fn bytecount_cases() {
        let count = |s: &str| crate::md::bytecount_newlines(s.as_bytes());
        assert_eq!(count(""), 0);
        assert_eq!(count("hello world"), 0);
        assert_eq!(count("a\nb\nc\n"), 3);
    }

    // ── TrackedTextBuffer ─────────────────────────────────────────────

    #[test]
    fn tracked_buffer_operations() {
        // Insert bumps seq.
        let mut text = Arc::new(String::from("hello"));
        let seq = Cell::new(0_u64);
        let mut buf = TrackedTextBuffer {
            text: &mut text,
            seq: &seq,
            line_ending: EndOfLine::Lf,
        };
        assert_eq!(egui::TextBuffer::insert_text(&mut buf, " world", 5), 6);
        assert_eq!(seq.get(), 1);
        assert_eq!(buf.as_str(), "hello world");

        // Empty insert does not bump seq.
        assert_eq!(egui::TextBuffer::insert_text(&mut buf, "", 0), 0);
        assert_eq!(seq.get(), 1);

        // Delete bumps seq.
        egui::TextBuffer::delete_char_range(&mut buf, 0..3);
        assert_eq!(seq.get(), 2);
        assert_eq!(buf.as_str(), "lo world");

        // Empty-range delete does not bump seq.
        egui::TextBuffer::delete_char_range(&mut buf, 2..2);
        assert_eq!(seq.get(), 2);
        assert_eq!(buf.as_str(), "lo world");

        // is_mutable.
        assert!(egui::TextBuffer::is_mutable(&buf));

        // Seq wraps at u64::MAX.
        let mut text2 = Arc::new(String::from("x"));
        let seq2 = Cell::new(u64::MAX);
        let mut buf2 = TrackedTextBuffer {
            text: &mut text2,
            seq: &seq2,
            line_ending: EndOfLine::Lf,
        };
        let _ = egui::TextBuffer::insert_text(&mut buf2, "y", 0);
        assert_eq!(seq2.get(), 0);
    }

    // ── Document defaults & helpers ───────────────────────────────────

    #[test]
    fn document_default_is_clean() {
        let doc = Document::default();
        assert!(!doc.dirty);
        assert!(!doc.stats_dirty);
        assert!(!doc.preview_dirty);
        assert_eq!(doc.edit_seq, 0);
        assert!(doc.last_edit_at.is_none());
        assert!(doc.path.is_none());
        assert!(Arc::ptr_eq(&doc.text, &doc.base_text));
    }

    #[test]
    fn document_title_and_path_label() {
        for (path, expected_title, expected_label) in [
            (None, "Untitled", "Unsaved"),
            (Some("/tmp/readme.md"), "readme.md", "/tmp/readme.md"),
        ] {
            let doc = Document {
                path: path.map(PathBuf::from),
                ..Default::default()
            };
            assert_eq!(doc.title(), expected_title);
            assert_eq!(doc.path_label(), expected_label);
        }
    }

    #[test]
    fn debounce_remaining_cases() {
        // No edit → None.
        assert!(
            Document::default()
                .debounce_remaining(Duration::from_millis(500))
                .is_none()
        );
        // Recent edit → Some.
        let recent = Document {
            last_edit_at: Some(Instant::now()),
            ..Default::default()
        };
        assert!(recent.debounce_remaining(Duration::from_secs(10)).is_some());
        // Expired → None.
        let old = Document {
            last_edit_at: Instant::now().checked_sub(Duration::from_secs(5)),
            ..Default::default()
        };
        assert!(old.debounce_remaining(Duration::from_millis(100)).is_none());
    }
}
