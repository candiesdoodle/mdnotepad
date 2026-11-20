//! Per-document state: one [`Tab`] per open document.
//!
//! Everything that belongs to a single document lives here: the text buffer,
//! disk sync / live reload, outline, side-by-side scroll bookkeeping, the
//! auto-save timer and the document's error banner.  App-wide settings (mode,
//! zoom, preference toggles, the find bar) stay on `RustdownApp`.

use std::{
    borrow::Cow,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use eframe::egui;

use crate::{
    AUTO_SAVE_INTERVAL, AUTO_SAVE_RETRY, BundledDoc, STATS_RECALC_DEBOUNCE, SideBySideScrollSource,
    default_image_uri_scheme,
    disk::{
        io::{DiskRevision, atomic_write_utf8, disk_revision, read_stable_utf8},
        sync::DiskSyncState,
    },
    document::{Document, DocumentStats},
    format,
    line_ending::{self, EndOfLine},
    markdown_file_dialog,
    md::MarkdownStyle,
    nav,
    search::replace_all_occurrences,
};

/// Stable identity of a tab.  It salts the egui widget ids of the editor and
/// preview, so every tab keeps its own cursor, undo history and scroll offset.
pub(crate) type TabId = u64;

static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);

fn next_tab_id() -> TabId {
    NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Default)]
pub(crate) struct PreviewStyleCache {
    pub(crate) style: Option<MarkdownStyle>,
    pub(crate) dark_mode: bool,
    pub(crate) colored: bool,
    pub(crate) image_uri: String,
}

pub(crate) struct Tab {
    pub(crate) id: TabId,
    pub(crate) doc: Document,
    /// How this document is shown.  While the tab is active the app's
    /// `mode` is the live value; it is stored back here when another tab
    /// becomes active.
    pub(crate) mode: crate::Mode,
    pub(crate) disk: DiskSyncState,
    /// Outline for this document.  Its `visible` flag is driven by the
    /// app-wide `RustdownApp::nav_visible` preference.
    pub(crate) nav: nav::panel::NavState,
    /// Error banner for this document (save / reload / watch failures).
    /// Per tab rather than app-wide because disk sync runs for every tab: a
    /// background reload must not clear, or be mistaken for, an error about
    /// the document the user is looking at.
    pub(crate) error: Option<String>,

    /// When the next auto-save should fire (`None` while doc is clean).
    pub(crate) auto_save_due_at: Option<Instant>,

    /// Last editor scroll byte offset observed by the side-by-side sync loop.
    pub(crate) last_sync_editor_byte: Option<usize>,
    /// Last preview scroll byte offset observed by the side-by-side sync loop.
    pub(crate) last_sync_preview_byte: Option<usize>,
    /// Set to `true` when `resolve_nav_scroll_target` applies a target this
    /// frame; prevents `sync_side_by_side_scroll` from overriding it.
    pub(crate) nav_scroll_applied_this_frame: bool,
    /// Current side-by-side sync source while a follower pane is animating.
    pub(crate) side_by_side_scroll_source: Option<SideBySideScrollSource>,
    /// Target scroll Y for the follower pane in `SideBySide` mode.
    pub(crate) side_by_side_scroll_target: Option<f32>,

    /// Cached preview style; rebuilt only when theme/colour-mode/URI changes.
    pub(crate) preview_style_cache: PreviewStyleCache,

    /// Id of the editor's scroll area as egui assigned it on the last render
    /// (it depends on the parent `Ui`, so it is recorded, not computed).
    pub(crate) editor_scroll_id: Option<egui::Id>,
    /// Misspellings found in this document (see `spell`).
    pub(crate) spelling: crate::spell::TabSpelling,
}

impl Default for Tab {
    fn default() -> Self {
        Self {
            id: next_tab_id(),
            doc: Document::default(),
            mode: crate::Mode::default(),
            disk: DiskSyncState::default(),
            nav: nav::panel::NavState::default(),
            error: None,
            auto_save_due_at: None,
            last_sync_editor_byte: None,
            last_sync_preview_byte: None,
            nav_scroll_applied_this_frame: false,
            side_by_side_scroll_source: None,
            side_by_side_scroll_target: None,
            preview_style_cache: PreviewStyleCache::default(),
            editor_scroll_id: None,
            spelling: crate::spell::TabSpelling::default(),
        }
    }
}

/// Canonical form of `path` used to detect "already open" files.  Falls back
/// to an absolute path for files that do not exist yet.
#[must_use]
pub(crate) fn canonical_path(path: &Path) -> PathBuf {
    path.canonicalize()
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

impl Tab {
    /// Open `path` into a new tab.  A missing file becomes an empty document
    /// bound to that path (saved on first Save).
    pub(crate) fn from_path(path: PathBuf) -> io::Result<Self> {
        let mut tab = Self::default();
        match read_stable_utf8(&path) {
            Ok((text, disk_rev)) => tab.load_document(path, text, Some(disk_rev)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tab.load_document(path, String::new(), None);
            }
            Err(err) => return Err(err),
        }
        Ok(tab)
    }

    /// A pathless tab holding a bundled (compile-time embedded) document.
    /// Save will trigger Save As.
    pub(crate) fn from_bundled(bundled: BundledDoc) -> Self {
        let mut tab = Self::default();
        tab.init_document(None, bundled.content().to_owned(), None);
        tab
    }

    #[must_use]
    pub(crate) fn title(&self) -> Cow<'_, str> {
        self.doc.title()
    }

    /// An empty, never-edited Untitled tab.  Opening a file replaces such a
    /// tab instead of adding a new one (as Notepad does).
    #[must_use]
    pub(crate) fn is_untouched(&self) -> bool {
        self.doc.path.is_none() && !self.doc.dirty && self.doc.text.is_empty()
    }

    /// Whether this tab shows the file at `canonical` (see [`canonical_path`]).
    #[must_use]
    pub(crate) fn has_path(&self, canonical: &Path) -> bool {
        self.doc
            .path
            .as_deref()
            .is_some_and(|path| canonical_path(path) == canonical)
    }

    /// egui id of this tab's editor `TextEdit`.  Use it with
    /// `egui::TextEdit::load_state` / `TextEditState::store` to read or set
    /// the cursor and selection.
    #[must_use]
    pub(crate) fn editor_id(&self) -> egui::Id {
        egui::Id::new("editor").with(self.id)
    }

    pub(crate) const fn clear_side_by_side_scroll_state(&mut self) {
        self.last_sync_editor_byte = None;
        self.last_sync_preview_byte = None;
        self.side_by_side_scroll_source = None;
        self.side_by_side_scroll_target = None;
    }

    pub(crate) const fn bump_edit_seq(&mut self) {
        self.doc.bump_edit_seq();
    }

    pub(crate) fn refresh_stats_now(&mut self) {
        // Force dirty so the Document method will recompute.
        self.doc.stats_dirty = true;
        self.doc.refresh_stats_if_dirty();
    }

    pub(crate) fn refresh_stats_if_due(&mut self, ctx: &egui::Context) {
        if !self.doc.stats_dirty {
            return;
        }
        if let Some(remaining) = self.doc.debounce_remaining(STATS_RECALC_DEBOUNCE) {
            ctx.request_repaint_after(remaining);
            return;
        }
        self.refresh_stats_now();
    }

    pub(crate) fn tick_auto_save(&mut self, ctx: &egui::Context, enabled: bool) {
        if !enabled {
            self.auto_save_due_at = None;
            return;
        }
        if !self.doc.dirty || self.doc.path.is_none() {
            self.auto_save_due_at = None;
            return;
        }
        if self.disk.awaits_answer() || self.disk.reload_in_flight {
            return;
        }

        let now = Instant::now();
        let due = *self
            .auto_save_due_at
            .get_or_insert(now + AUTO_SAVE_INTERVAL);
        if now < due {
            if let Some(remaining) = due.checked_duration_since(now) {
                ctx.request_repaint_after(remaining);
            }
            return;
        }

        if self.save_doc(false) {
            self.auto_save_due_at = None;
        } else {
            self.auto_save_due_at = Some(now + AUTO_SAVE_RETRY);
        }
    }

    pub(crate) fn note_text_changed(&mut self, defer_stats_recalc: bool) {
        self.doc.mark_text_changed();
        if !defer_stats_recalc {
            self.refresh_stats_now();
        }
    }

    pub(crate) fn replace_all_matches(&mut self, query: &str, replacement: &str) -> usize {
        let replacement = line_ending::normalize(replacement, self.doc.line_ending);
        let (text, replaced) = replace_all_occurrences(self.doc.text.as_str(), query, &replacement);
        if let Cow::Owned(text) = text {
            self.doc.text = Arc::new(text);
            self.bump_edit_seq();
            self.note_text_changed(false);
        }
        replaced
    }

    /// Format the document.  Line breaks are written in the document's
    /// style, unless `.editorconfig` sets `end_of_line`: then that becomes
    /// the document's style.
    pub(crate) fn format_document(&mut self) {
        let mut options = format::options_for_path(self.doc.path.as_deref());
        self.doc.line_ending = *options.end_of_line.get_or_insert(self.doc.line_ending);
        let formatted = format::format_markdown(self.doc.text.as_str(), options);
        if formatted == self.doc.text.as_str() {
            return;
        }

        self.doc.text = Arc::new(formatted);
        self.bump_edit_seq();
        self.note_text_changed(false);
    }

    /// Common initialisation for loading a new document (file-based or
    /// bundled).  The text is kept as it is; its line-ending style is the
    /// dominant one in it (without line breaks: the style the tab had).
    pub(crate) fn init_document(
        &mut self,
        path: Option<PathBuf>,
        text: String,
        disk_rev: Option<DiskRevision>,
    ) {
        let text = Arc::new(text);
        let base_text = text.clone();
        let image_uri_scheme = path
            .as_deref()
            .map_or_else(String::new, |p| default_image_uri_scheme(Some(p)));
        let next_seq = self.doc.edit_seq.wrapping_add(1);
        let line_ending = EndOfLine::detect(text.as_str()).unwrap_or(self.doc.line_ending);
        self.doc = Document {
            path,
            image_uri_scheme,
            stats: DocumentStats::from_text(text.as_str()),
            text,
            line_ending,
            base_text,
            disk_rev,
            stats_dirty: false,
            preview_dirty: false,
            dirty: false,
            preview_cache: crate::md::MarkdownCache::default(),
            last_edit_at: None,
            edit_seq: next_seq,
            editor_galley_cache: None,
            formatted_analysis: None,
        };
        self.nav.invalidate_outline();
        self.clear_side_by_side_scroll_state();
    }

    pub(crate) fn load_document(
        &mut self,
        path: PathBuf,
        text: String,
        disk_rev: Option<DiskRevision>,
    ) {
        self.init_document(Some(path), text, disk_rev);
    }

    pub(crate) fn save_path_choice(&self, save_as: bool) -> Option<(PathBuf, bool)> {
        if !save_as && let Some(path) = self.doc.path.clone() {
            return Some((path, false));
        }
        markdown_file_dialog().save_file().map(|path| (path, true))
    }

    /// Save the document (Save As when `save_as` or the tab is Untitled).
    /// Returns `false` when the dialog was cancelled, the write failed, or a
    /// question about the changed file must be answered first.
    pub(crate) fn save_doc(&mut self, save_as: bool) -> bool {
        let Some((path, update_doc_path)) = self.save_path_choice(save_as) else {
            return false;
        };

        let saving_to_current_path = self.doc.path.as_deref() == Some(path.as_path());

        if !self.disk.awaits_answer() && saving_to_current_path {
            match read_stable_utf8(&path) {
                Ok((disk_text, disk_rev)) => self.incorporate_disk_text(disk_text, disk_rev),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    self.error
                        .get_or_insert_with(|| format!("Pre-save reload failed: {err}"));
                }
            }
        }

        if self.disk.awaits_answer() {
            return false;
        }

        match atomic_write_utf8(&path, self.doc.text.as_str()) {
            Ok(()) => {
                if update_doc_path {
                    self.doc.path = Some(path.clone());
                    self.doc.image_uri_scheme = default_image_uri_scheme(Some(path.as_path()));
                }
                self.doc.dirty = false;
                self.doc.base_text = self.doc.text.clone();
                self.doc.disk_rev = disk_revision(&path).ok();

                self.error = None;
                self.reset_disk_sync_state();
                true
            }
            Err(err) => {
                self.error = Some(format!("Save failed: {err}"));
                false
            }
        }
    }
}
