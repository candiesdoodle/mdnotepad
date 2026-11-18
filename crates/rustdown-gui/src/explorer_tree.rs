//! Explorer: the folder tree model (no UI).
//!
//! The tree is lazy: a directory is read only when it is shown (the root,
//! or an expanded folder whose ancestors are all expanded).  Reads run on
//! background threads and land in a listing cache; the UI draws a flat list
//! of [`Row`]s rebuilt from that cache when something changes.  A recursive
//! `notify` watcher on the root invalidates only the affected listings,
//! debounced.

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use eframe::egui;
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::{
    explorer_fs::{Listing, display_path, is_visible_relative_path, read_listing},
    tab::canonical_path,
};

/// Quiet period after the last file-system event before listings refresh.
pub(crate) const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// Upper bound on how long a stream of events can delay a refresh.
const WATCH_MAX_DELAY: Duration = Duration::from_millis(1000);
/// Fallback repaint interval while listings are loading.
const LOADING_POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowKind {
    Folder {
        expanded: bool,
    },
    File {
        markdown: bool,
    },
    /// The folder's listing is being read.
    Loading,
    /// The folder has nothing to show (with the current filter).
    Empty,
    /// The folder could not be read.
    Error(String),
}

/// One line of the flattened tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    /// Nesting level; top-level entries of the root are 0.
    pub(crate) depth: usize,
    pub(crate) kind: RowKind,
    /// Entry name; empty for status rows.
    pub(crate) name: String,
    /// The entry's path; for status rows, the folder they describe.
    pub(crate) path: PathBuf,
}

impl Row {
    const fn status(depth: usize, kind: RowKind, dir: PathBuf) -> Self {
        Self {
            depth,
            kind,
            name: String::new(),
            path: dir,
        }
    }

    /// A file or folder (not a Loading / Empty / Error line).
    #[must_use]
    pub(crate) const fn is_entry(&self) -> bool {
        matches!(self.kind, RowKind::Folder { .. } | RowKind::File { .. })
    }
}

struct ListingResult {
    dir: PathBuf,
    id: u64,
    listing: Listing,
}

#[derive(Debug, PartialEq, Eq)]
enum WatchMsg {
    /// Paths under the root that changed; `rescan` means events were lost.
    Changed {
        paths: Vec<PathBuf>,
        rescan: bool,
    },
    Failed(String),
}

/// A running recursive watch.  Dropping it stops the watcher thread.
struct RootWatch {
    _stop: mpsc::Sender<()>,
    events: mpsc::Receiver<WatchMsg>,
}

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ExplorerTree {
    /// The folder as the user chose it (absolute, no verbatim prefix).
    /// Every row path is this joined with entry names.
    root: PathBuf,
    /// `canonical_path(root)`, for matching tab paths to rows.
    canonical_root: PathBuf,

    /// Cached directory listings (loaded or failed).
    listings: HashMap<PathBuf, Listing>,
    /// Listings being read: directory → id of the latest request.  Older
    /// results (from before an invalidation) are dropped on arrival.
    requests: HashMap<PathBuf, u64>,
    next_request_id: u64,
    listing_tx: mpsc::Sender<ListingResult>,
    listing_rx: mpsc::Receiver<ListingResult>,
    /// Expanded folders.  Kept when a parent collapses, as in VS Code.
    expanded: HashSet<PathBuf>,
    /// Lets background threads wake the UI; known after the first frame.
    repaint: Option<egui::Context>,

    watch: Option<RootWatch>,
    watch_started: bool,
    watch_error: Option<String>,
    /// Paths reported by the watcher since the last refresh.
    changed: HashSet<PathBuf>,
    rescan: bool,
    flush_at: Option<Instant>,
    first_change_at: Option<Instant>,
    root_missing: bool,

    rows: Vec<Row>,
    rows_valid: bool,
    rows_show_all: bool,
    /// Row path of the active tab's file (highlighted).
    selected: Option<PathBuf>,
    /// Row to scroll into view once it exists.
    scroll_target: Option<PathBuf>,
    /// Scroll offset and height of the tree viewport on the last frame.
    pub(crate) viewport: (f32, f32),
}

impl ExplorerTree {
    /// Start showing `root` (see `explorer_fs::normalize_root`); its
    /// listing starts loading immediately.
    pub(crate) fn new(root: PathBuf) -> Self {
        let canonical_root = canonical_path(&root);
        let (listing_tx, listing_rx) = mpsc::channel();
        let mut tree = Self {
            root,
            canonical_root,
            listings: HashMap::new(),
            requests: HashMap::new(),
            next_request_id: 0,
            listing_tx,
            listing_rx,
            expanded: HashSet::new(),
            repaint: None,
            watch: None,
            watch_started: false,
            watch_error: None,
            changed: HashSet::new(),
            rescan: false,
            flush_at: None,
            first_change_at: None,
            root_missing: false,
            rows: Vec::new(),
            rows_valid: false,
            rows_show_all: false,
            selected: None,
            scroll_target: None,
            viewport: (0.0, 0.0),
        };
        tree.request(vec![tree.root.clone()]);
        tree
    }

    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The root folder was deleted, renamed or is otherwise unavailable.
    #[must_use]
    pub(crate) const fn root_missing(&self) -> bool {
        self.root_missing
    }

    /// Why live refresh is not working, if it isn't.
    #[must_use]
    pub(crate) fn watch_error(&self) -> Option<&str> {
        self.watch_error.as_deref()
    }

    #[must_use]
    pub(crate) fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    #[must_use]
    pub(crate) fn is_loading(&self) -> bool {
        !self.requests.is_empty()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_expanded(&self, dir: &Path) -> bool {
        self.expanded.contains(dir)
    }

    /// The flattened rows; call [`Self::prepare_rows`] first each frame.
    #[must_use]
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    // ── Loading ─────────────────────────────────────────────────────────

    /// Read `dirs` on a background thread (one thread per batch).
    fn request(&mut self, dirs: Vec<PathBuf>) {
        if dirs.is_empty() {
            return;
        }
        let mut batch = Vec::with_capacity(dirs.len());
        for dir in dirs {
            self.next_request_id += 1;
            self.requests.insert(dir.clone(), self.next_request_id);
            batch.push((dir, self.next_request_id));
        }
        let dirs: Vec<PathBuf> = batch.iter().map(|(dir, _)| dir.clone()).collect();
        let tx = self.listing_tx.clone();
        let repaint = self.repaint.clone();
        let spawned = std::thread::Builder::new()
            .name("explorer-list".to_owned())
            .spawn(move || {
                for (dir, id) in batch {
                    let listing = read_listing(&dir);
                    if tx.send(ListingResult { dir, id, listing }).is_err() {
                        return;
                    }
                    if let Some(ctx) = &repaint {
                        ctx.request_repaint();
                    }
                }
            });
        if let Err(err) = spawned {
            for dir in dirs {
                self.requests.remove(&dir);
                self.listings
                    .insert(dir, Err(format!("Can't read folder: {err}")));
            }
            self.rows_valid = false;
        }
    }

    fn accept_listing(&mut self, result: ListingResult) {
        if self.requests.get(&result.dir) != Some(&result.id) {
            return; // superseded by a newer request, or no longer wanted
        }
        self.requests.remove(&result.dir);
        if result.dir == self.root && result.listing.is_err() && !self.root.is_dir() {
            self.root_missing = true;
        }
        self.listings.insert(result.dir, result.listing);
        self.rows_valid = false;
    }

    /// Apply finished listings and watcher events (refreshing listings once
    /// the debounce has passed).  Pass the egui context once it exists: it
    /// starts the watcher and lets background threads request repaints.
    /// Returns how soon to poll again, if something is pending.
    pub(crate) fn poll(&mut self, ctx: Option<&egui::Context>, now: Instant) -> Option<Duration> {
        if let Some(ctx) = ctx {
            if self.repaint.is_none() {
                self.repaint = Some(ctx.clone());
            }
            if !self.watch_started && !self.root_missing {
                self.start_watch(ctx);
            }
        }
        while let Ok(result) = self.listing_rx.try_recv() {
            self.accept_listing(result);
        }
        self.drain_watch(now);
        if self.flush_at.is_some_and(|at| now >= at) {
            self.flush_changes();
        }

        let mut wake = self.flush_at.map(|at| at.saturating_duration_since(now));
        if self.is_loading() {
            wake = Some(wake.map_or(LOADING_POLL, |w| w.min(LOADING_POLL)));
        }
        wake
    }

    // ── Watching ────────────────────────────────────────────────────────

    fn start_watch(&mut self, ctx: &egui::Context) {
        self.watch_started = true;
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (tx, rx) = mpsc::channel();
        let root = self.root.clone();
        let ctx = ctx.clone();
        // Setting up a recursive watch can walk the whole tree (inotify), so
        // it happens off the UI thread.  The thread then parks until the
        // explorer drops `stop_tx`, which also drops the watcher.
        let spawned = std::thread::Builder::new()
            .name("explorer-watch".to_owned())
            .spawn(move || {
                let handler_tx = tx.clone();
                let handler_root = root.clone();
                let handler_ctx = ctx.clone();
                let handler = move |res: notify::Result<Event>| {
                    let msg = match res {
                        Ok(event) => match watch_message(&handler_root, event) {
                            Some(msg) => msg,
                            None => return,
                        },
                        Err(err) => WatchMsg::Failed(err.to_string()),
                    };
                    if handler_tx.send(msg).is_ok() {
                        handler_ctx.request_repaint();
                    }
                };
                let mut watcher = match notify::recommended_watcher(handler) {
                    Ok(watcher) => watcher,
                    Err(err) => {
                        let _ = tx.send(WatchMsg::Failed(err.to_string()));
                        ctx.request_repaint();
                        return;
                    }
                };
                if let Err(err) = watcher.watch(&root, RecursiveMode::Recursive) {
                    let _ = tx.send(WatchMsg::Failed(err.to_string()));
                    ctx.request_repaint();
                    return;
                }
                let _ = stop_rx.recv();
            });
        match spawned {
            Ok(_) => {
                self.watch = Some(RootWatch {
                    _stop: stop_tx,
                    events: rx,
                });
            }
            Err(err) => self.watch_error = Some(err.to_string()),
        }
    }

    fn drain_watch(&mut self, now: Instant) {
        let Some(watch) = &self.watch else {
            return;
        };
        let mut paths = Vec::new();
        let mut rescan = false;
        let mut changed = false;
        let mut disconnected = false;
        loop {
            match watch.events.try_recv() {
                Ok(WatchMsg::Changed {
                    paths: event_paths,
                    rescan: event_rescan,
                }) => {
                    paths.extend(event_paths);
                    rescan |= event_rescan;
                    changed = true;
                }
                Ok(WatchMsg::Failed(err)) => self.watch_error = Some(err),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            // The watcher could not start (or stopped); Refresh retries.
            self.watch = None;
            self.check_root();
        }
        if changed {
            self.note_changes(paths, rescan, now);
        }
    }

    /// Record changed paths; listings refresh after [`WATCH_DEBOUNCE`].
    pub(crate) fn note_changes(&mut self, paths: Vec<PathBuf>, rescan: bool, now: Instant) {
        self.changed.extend(paths);
        self.rescan |= rescan;
        let first = *self.first_change_at.get_or_insert(now);
        self.flush_at = Some((now + WATCH_DEBOUNCE).min(first + WATCH_MAX_DELAY));
    }

    fn flush_changes(&mut self) {
        self.flush_at = None;
        self.first_change_at = None;
        let dirs: Vec<PathBuf> = if std::mem::take(&mut self.rescan) {
            self.changed.clear();
            self.listings
                .keys()
                .chain(self.requests.keys())
                .cloned()
                .collect()
        } else {
            let mut dirs = Vec::with_capacity(self.changed.len() * 2);
            for path in self.changed.drain() {
                // A changed entry alters its parent's listing; a changed
                // folder may also have its own listing cached.
                if let Some(parent) = path.parent() {
                    dirs.push(parent.to_path_buf());
                }
                dirs.push(path);
            }
            dirs
        };
        self.invalidate(dirs);
        self.check_root();
    }

    /// Re-read the given directories if they are shown; forget them if
    /// they are cached but hidden (they are re-read when expanded).
    /// Directories that were never read are ignored.
    pub(crate) fn invalidate(&mut self, dirs: impl IntoIterator<Item = PathBuf>) {
        let mut reload = HashSet::new();
        for dir in dirs {
            if !self.listings.contains_key(&dir) && !self.requests.contains_key(&dir) {
                continue;
            }
            if self.is_shown(&dir) {
                reload.insert(dir);
            } else {
                self.listings.remove(&dir);
                self.requests.remove(&dir);
            }
        }
        self.rows_valid = false;
        self.request(reload.into_iter().collect());
    }

    /// Mark the root missing if it no longer exists.
    pub(crate) fn check_root(&mut self) {
        if !self.root.is_dir() {
            self.root_missing = true;
        }
    }

    // ── User actions ────────────────────────────────────────────────────

    /// Manual refresh: re-check the root, re-read every shown folder, drop
    /// hidden cached ones, and restart live refresh if it had stopped.
    pub(crate) fn refresh_all(&mut self) {
        self.root_missing = !self.root.is_dir();
        if self.root_missing {
            return;
        }
        let (shown, hidden): (Vec<PathBuf>, Vec<PathBuf>) = self
            .listings
            .keys()
            .cloned()
            .partition(|dir| self.is_shown(dir));
        for dir in hidden {
            self.listings.remove(&dir);
        }
        self.request(shown);
        if self.watch.is_none() {
            self.watch_started = false;
            self.watch_error = None;
        }
        self.rows_valid = false;
    }

    /// Re-read one folder (if shown).
    pub(crate) fn refresh_dir(&mut self, dir: &Path) {
        if self.is_shown(dir) {
            self.request(vec![dir.to_path_buf()]);
        } else {
            self.listings.remove(dir);
        }
        self.rows_valid = false;
    }

    pub(crate) fn toggle_expanded(&mut self, dir: &Path) {
        if !self.expanded.remove(dir) {
            self.expanded.insert(dir.to_path_buf());
        }
        self.rows_valid = false;
    }

    pub(crate) fn expand(&mut self, dir: &Path) {
        if dir != self.root && self.expanded.insert(dir.to_path_buf()) {
            self.rows_valid = false;
        }
    }

    pub(crate) fn collapse_all(&mut self) {
        self.expanded.clear();
        self.rows_valid = false;
    }

    /// Whether `dir` is visible with its contents listed: the root, or an
    /// expanded folder whose ancestors (below the root) are all expanded.
    fn is_shown(&self, dir: &Path) -> bool {
        if dir == self.root {
            return true;
        }
        dir.starts_with(&self.root)
            && dir
                .ancestors()
                .take_while(|ancestor| *ancestor != self.root)
                .all(|ancestor| self.expanded.contains(ancestor))
    }

    // ── Active file ─────────────────────────────────────────────────────

    /// The row path of `file` if the tree can show it (inside the root, not
    /// in a skipped folder).  Matches by plain prefix first (no system
    /// call), then by canonical path (other spellings, `\\?\`, links).
    #[must_use]
    pub(crate) fn tree_path_for(&self, file: &Path) -> Option<PathBuf> {
        let direct = file
            .strip_prefix(&self.root)
            .ok()
            .filter(|rel| rel.components().all(|c| matches!(c, Component::Normal(_))));
        let rel = match direct {
            Some(rel) => rel.to_path_buf(),
            None => canonical_path(file)
                .strip_prefix(&self.canonical_root)
                .ok()?
                .to_path_buf(),
        };
        (!rel.as_os_str().is_empty() && is_visible_relative_path(&rel)).then(|| self.root.join(rel))
    }

    /// Highlight `target` (a row path from [`Self::tree_path_for`]), expand
    /// its ancestors and scroll it into view once its row exists.  `None`
    /// clears the highlight.
    pub(crate) fn reveal(&mut self, target: Option<PathBuf>) {
        self.selected.clone_from(&target);
        self.scroll_target = None;
        let Some(target) = target.filter(|t| t.starts_with(&self.root) && *t != self.root) else {
            return;
        };
        for ancestor in target.ancestors().skip(1) {
            if ancestor == self.root {
                break;
            }
            self.expand(ancestor);
        }
        self.scroll_target = Some(target);
    }

    // ── Rows ────────────────────────────────────────────────────────────

    /// Rebuild the flattened rows if anything changed, and start reading
    /// any shown folder that has no listing yet.
    pub(crate) fn prepare_rows(&mut self, show_all: bool) {
        if self.rows_valid && self.rows_show_all == show_all {
            return;
        }
        let mut rows = Vec::with_capacity(self.rows.len());
        let mut missing = Vec::new();
        self.push_rows(&self.root, 0, show_all, &mut rows, &mut missing);
        self.rows = rows;
        self.rows_valid = true;
        self.rows_show_all = show_all;
        self.request(missing);
    }

    fn push_rows(
        &self,
        dir: &Path,
        depth: usize,
        show_all: bool,
        rows: &mut Vec<Row>,
        missing: &mut Vec<PathBuf>,
    ) {
        match self.listings.get(dir) {
            None => {
                if !self.requests.contains_key(dir) {
                    missing.push(dir.to_path_buf());
                }
                rows.push(Row::status(depth, RowKind::Loading, dir.to_path_buf()));
            }
            Some(Err(message)) => {
                rows.push(Row::status(
                    depth,
                    RowKind::Error(message.clone()),
                    dir.to_path_buf(),
                ));
            }
            Some(Ok(entries)) => {
                let before = rows.len();
                for entry in entries
                    .iter()
                    .filter(|entry| entry.is_dir || entry.is_markdown || show_all)
                {
                    let expanded = entry.is_dir && self.expanded.contains(&entry.path);
                    let kind = if entry.is_dir {
                        RowKind::Folder { expanded }
                    } else {
                        RowKind::File {
                            markdown: entry.is_markdown,
                        }
                    };
                    rows.push(Row {
                        depth,
                        kind,
                        name: entry.name.clone(),
                        path: entry.path.clone(),
                    });
                    if expanded {
                        self.push_rows(&entry.path, depth + 1, show_all, rows, missing);
                    }
                }
                if rows.len() == before {
                    rows.push(Row::status(depth, RowKind::Empty, dir.to_path_buf()));
                }
            }
        }
    }

    /// Index of the row to scroll into view, once (after `prepare_rows`).
    /// Gives up when nothing is loading and the target has no row (e.g. a
    /// file hidden by the Markdown filter).
    pub(crate) fn take_scroll_index(&mut self) -> Option<usize> {
        let target = self.scroll_target.as_ref()?;
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| row.is_entry() && row.path == *target)
        {
            self.scroll_target = None;
            return Some(index);
        }
        if self.rows_valid && !self.is_loading() {
            self.scroll_target = None;
        }
        None
    }

    /// Tooltip text for a row: its path as the user would write it.
    #[must_use]
    pub(crate) fn row_tooltip(row: &Row) -> String {
        display_path(&row.path)
    }
}

/// New scroll offset that brings row `index` into view (centred), or `None`
/// when it is already fully visible.  `viewport` is (offset, height).
#[must_use]
pub(crate) fn scroll_offset_to_show(
    index: usize,
    row_height: f32,
    viewport: (f32, f32),
) -> Option<f32> {
    let (offset, height) = viewport;
    let top = index as f32 * row_height;
    let bottom = top + row_height;
    if height > 0.0 && top >= offset && bottom <= offset + height {
        return None;
    }
    Some((top - (height - row_height).max(0.0) / 2.0).max(0.0))
}

/// Turn a watcher event into a message, dropping noise: access events and
/// paths the tree never shows (outside the root, dot folders, heavy dirs).
fn watch_message(root: &Path, event: Event) -> Option<WatchMsg> {
    if matches!(event.kind, EventKind::Access(_)) {
        return None;
    }
    let rescan = event.need_rescan();
    let paths: Vec<PathBuf> = event
        .paths
        .into_iter()
        .filter(|path| path.strip_prefix(root).is_ok_and(is_visible_relative_path))
        .collect();
    if paths.is_empty() && !rescan {
        return None;
    }
    Some(WatchMsg::Changed { paths, rescan })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use notify::event::{AccessKind, CreateKind, Flag};

    use super::*;
    use crate::explorer_fs::test_temp_dir;

    /// Poll until no listing is loading (or give up after a few seconds).
    fn wait_idle(tree: &mut ExplorerTree, show_all: bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let _ = tree.poll(None, Instant::now());
            tree.prepare_rows(show_all);
            if !tree.is_loading() || Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn names(tree: &ExplorerTree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|row| {
                let indent = "  ".repeat(row.depth);
                match &row.kind {
                    RowKind::Folder { expanded: true } => format!("{indent}v {}", row.name),
                    RowKind::Folder { expanded: false } => format!("{indent}> {}", row.name),
                    RowKind::File { .. } => format!("{indent}{}", row.name),
                    RowKind::Loading => format!("{indent}(loading)"),
                    RowKind::Empty => format!("{indent}(empty)"),
                    RowKind::Error(message) => format!("{indent}(error: {message})"),
                }
            })
            .collect()
    }

    fn sample_tree(name: &str) -> PathBuf {
        let dir = test_temp_dir(name);
        let _ = fs::create_dir_all(dir.join("docs").join("deep"));
        let _ = fs::create_dir_all(dir.join("empty"));
        let _ = fs::write(dir.join("readme.md"), "# r");
        let _ = fs::write(dir.join("notes.txt"), "n");
        let _ = fs::write(dir.join("docs").join("guide10.md"), "g");
        let _ = fs::write(dir.join("docs").join("guide2.md"), "g");
        let _ = fs::write(dir.join("docs").join("deep").join("x.md"), "x");
        dir
    }

    #[test]
    fn lazy_tree_lists_expands_and_filters() {
        let dir = sample_tree("tree-basic");
        let mut tree = ExplorerTree::new(dir.clone());
        assert!(tree.is_loading());
        wait_idle(&mut tree, false);
        assert_eq!(names(&tree), ["> docs", "> empty", "readme.md"]);
        // Only the root was read.
        assert_eq!(tree.listings.len(), 1);

        tree.toggle_expanded(&dir.join("docs"));
        tree.prepare_rows(false);
        assert_eq!(
            names(&tree),
            ["v docs", "  (loading)", "> empty", "readme.md"]
        );
        wait_idle(&mut tree, false);
        assert_eq!(
            names(&tree),
            [
                "v docs",
                "  > deep",
                "  guide2.md",
                "  guide10.md",
                "> empty",
                "readme.md"
            ]
        );

        tree.toggle_expanded(&dir.join("empty"));
        wait_idle(&mut tree, true);
        assert_eq!(
            names(&tree),
            [
                "v docs",
                "  > deep",
                "  guide2.md",
                "  guide10.md",
                "v empty",
                "  (empty)",
                "notes.txt",
                "readme.md"
            ]
        );
        assert_eq!(tree.rows()[6].kind, RowKind::File { markdown: false });

        tree.collapse_all();
        tree.prepare_rows(false);
        assert_eq!(names(&tree), ["> docs", "> empty", "readme.md"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_and_unreadable_folders_show_inline_rows() {
        let dir = test_temp_dir("tree-errors");
        let mut tree = ExplorerTree::new(dir.join("gone"));
        wait_idle(&mut tree, false);
        assert!(tree.root_missing());
        assert_eq!(names(&tree), ["(error: Folder not found)"]);

        // A shown folder whose listing fails gets an error row.
        let _ = fs::create_dir_all(dir.join("sub"));
        let mut tree = ExplorerTree::new(dir.clone());
        wait_idle(&mut tree, false);
        let _ = fs::remove_dir_all(dir.join("sub"));
        tree.toggle_expanded(&dir.join("sub"));
        wait_idle(&mut tree, false);
        assert_eq!(names(&tree), ["v sub", "  (error: Folder not found)"]);
        assert!(!tree.root_missing());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_listing_results_are_ignored() {
        let dir = sample_tree("tree-stale");
        let mut tree = ExplorerTree::new(dir.clone());
        wait_idle(&mut tree, false);
        // A result for an older request id must not overwrite the cache.
        tree.requests.insert(dir.clone(), 99);
        tree.accept_listing(ListingResult {
            dir: dir.clone(),
            id: 98,
            listing: Ok(Vec::new()),
        });
        assert!(
            tree.listings
                .get(&dir)
                .is_some_and(|l| l.as_ref().is_ok_and(|e| !e.is_empty()))
        );
        tree.accept_listing(ListingResult {
            dir: dir.clone(),
            id: 99,
            listing: Ok(Vec::new()),
        });
        tree.prepare_rows(false);
        assert_eq!(names(&tree), ["(empty)"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn changes_invalidate_only_affected_listings_after_debounce() {
        let dir = sample_tree("tree-invalidate");
        let mut tree = ExplorerTree::new(dir.clone());
        tree.toggle_expanded(&dir.join("docs"));
        wait_idle(&mut tree, false);
        tree.toggle_expanded(&dir.join("docs").join("deep"));
        wait_idle(&mut tree, false);
        tree.toggle_expanded(&dir.join("docs").join("deep"));
        wait_idle(&mut tree, false);
        assert_eq!(tree.listings.len(), 3);

        let _ = fs::write(dir.join("docs").join("guide3.md"), "g");
        let _ = fs::write(dir.join("docs").join("deep").join("y.md"), "y");
        let now = Instant::now();
        tree.note_changes(
            vec![
                dir.join("docs").join("guide3.md"),
                dir.join("docs").join("deep").join("y.md"),
            ],
            false,
            now,
        );
        // Nothing happens before the debounce.
        assert_eq!(tree.poll(None, now), Some(WATCH_DEBOUNCE));
        assert!(!tree.is_loading());
        // After it: the shown folder is re-read, the hidden one forgotten,
        // the root (unaffected) is kept.
        let _ = tree.poll(None, now + WATCH_DEBOUNCE);
        assert!(tree.requests.contains_key(&dir.join("docs")));
        assert!(!tree.requests.contains_key(&dir));
        assert!(!tree.listings.contains_key(&dir.join("docs").join("deep")));
        wait_idle(&mut tree, false);
        assert_eq!(
            names(&tree),
            [
                "v docs",
                "  > deep",
                "  guide2.md",
                "  guide3.md",
                "  guide10.md",
                "> empty",
                "readme.md"
            ]
        );

        // A burst of events cannot postpone the refresh forever.
        let start = Instant::now();
        for step in 0..10 {
            tree.note_changes(
                vec![dir.join("a.md")],
                false,
                start + Duration::from_millis(step * 200),
            );
        }
        assert_eq!(tree.flush_at, Some(start + WATCH_MAX_DELAY));

        // Deleting the root is noticed on the next refresh.
        let _ = fs::remove_dir_all(&dir);
        tree.note_changes(vec![dir.clone()], false, start);
        let _ = tree.poll(None, start + WATCH_MAX_DELAY);
        assert!(tree.root_missing());
        let _ = fs::create_dir_all(&dir);
        tree.refresh_all();
        assert!(!tree.root_missing());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rescan_reloads_everything_cached() {
        let dir = sample_tree("tree-rescan");
        let mut tree = ExplorerTree::new(dir.clone());
        tree.toggle_expanded(&dir.join("docs"));
        wait_idle(&mut tree, false);
        let now = Instant::now();
        tree.note_changes(Vec::new(), true, now);
        let _ = tree.poll(None, now + WATCH_DEBOUNCE);
        assert!(tree.requests.contains_key(&dir));
        assert!(tree.requests.contains_key(&dir.join("docs")));
        wait_idle(&mut tree, false);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manual_refresh_rereads_shown_folders() {
        let dir = sample_tree("tree-refresh");
        let mut tree = ExplorerTree::new(dir.clone());
        tree.toggle_expanded(&dir.join("docs"));
        wait_idle(&mut tree, false);
        let _ = fs::write(dir.join("docs").join("added.md"), "a");
        let _ = fs::write(dir.join("added-root.md"), "a");
        tree.refresh_all();
        wait_idle(&mut tree, false);
        let rows = names(&tree);
        assert!(rows.contains(&"  added.md".to_owned()), "{rows:?}");
        assert!(rows.contains(&"added-root.md".to_owned()), "{rows:?}");

        let _ = fs::write(dir.join("docs").join("one-more.md"), "a");
        tree.refresh_dir(&dir.join("docs"));
        wait_idle(&mut tree, false);
        assert!(names(&tree).contains(&"  one-more.md".to_owned()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reveal_expands_ancestors_and_scrolls_to_the_row() {
        let dir = sample_tree("tree-reveal");
        let mut tree = ExplorerTree::new(dir.clone());
        wait_idle(&mut tree, false);
        let file = dir.join("docs").join("deep").join("x.md");
        let target = tree.tree_path_for(&file);
        assert_eq!(target.as_deref(), Some(file.as_path()));
        tree.reveal(target);
        assert!(tree.is_expanded(&dir.join("docs")));
        assert!(tree.is_expanded(&dir.join("docs").join("deep")));
        assert!(!tree.is_expanded(&dir));
        wait_idle(&mut tree, false);
        assert_eq!(tree.selected(), Some(file.as_path()));
        let index = tree.take_scroll_index();
        assert_eq!(index, Some(2));
        assert_eq!(tree.rows()[2].path, file);
        assert_eq!(tree.take_scroll_index(), None, "scrolls once");

        // A file hidden by the Markdown filter: highlighted, no scroll.
        let txt = dir.join("notes.txt");
        tree.reveal(tree.tree_path_for(&txt));
        tree.prepare_rows(false);
        assert_eq!(tree.take_scroll_index(), None);
        assert_eq!(tree.scroll_target, None);

        // Paths outside the root, in skipped folders, or the root itself.
        assert_eq!(tree.tree_path_for(&std::env::temp_dir().join("x.md")), None);
        assert_eq!(tree.tree_path_for(&dir.join(".git").join("x.md")), None);
        assert_eq!(
            tree.tree_path_for(&dir.join("node_modules").join("x.md")),
            None
        );
        assert_eq!(tree.tree_path_for(&dir), None);
        // Other spellings of the same file resolve to the row path.
        let dotted = dir.join("docs").join("..").join("readme.md");
        assert_eq!(tree.tree_path_for(&dotted), Some(dir.join("readme.md")));

        tree.reveal(None);
        assert_eq!(tree.selected(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scroll_offset_only_when_the_row_is_not_visible() {
        let viewport = (100.0, 200.0);
        assert_eq!(scroll_offset_to_show(5, 20.0, viewport), None);
        assert_eq!(scroll_offset_to_show(0, 20.0, viewport), Some(0.0));
        assert_eq!(scroll_offset_to_show(20, 20.0, viewport), Some(310.0));
        // Unknown viewport (first frame): scroll.
        assert_eq!(scroll_offset_to_show(3, 20.0, (0.0, 0.0)), Some(60.0));
    }

    #[test]
    fn watch_messages_drop_noise() {
        let root = PathBuf::from("/notes");
        let event = |kind, path: &str| Event::new(kind).add_path(PathBuf::from(path));
        let create = EventKind::Create(CreateKind::File);
        assert_eq!(
            watch_message(&root, event(create, "/notes/a.md")),
            Some(WatchMsg::Changed {
                paths: vec![PathBuf::from("/notes/a.md")],
                rescan: false
            })
        );
        assert_eq!(
            watch_message(&root, event(create, "/notes/.git/index")),
            None
        );
        assert_eq!(
            watch_message(&root, event(create, "/notes/web/node_modules/x/y.md")),
            None
        );
        assert_eq!(watch_message(&root, event(create, "/elsewhere/a.md")), None);
        assert_eq!(
            watch_message(
                &root,
                event(EventKind::Access(AccessKind::Any), "/notes/a.md")
            ),
            None
        );
        assert_eq!(
            watch_message(&root, Event::new(EventKind::Other).set_flag(Flag::Rescan)),
            Some(WatchMsg::Changed {
                paths: Vec::new(),
                rescan: true
            })
        );
        // The root itself (deleted / renamed) is relevant.
        assert!(watch_message(&root, event(EventKind::Any, "/notes")).is_some());
    }

    #[test]
    fn live_refresh_picks_up_new_files() {
        let dir = sample_tree("tree-watch");
        let ctx = egui::Context::default();
        let mut tree = ExplorerTree::new(dir.clone());
        let _ = tree.poll(Some(&ctx), Instant::now());
        wait_idle(&mut tree, false);
        // The watch is set up on a background thread; keep creating files
        // until one shows up, so a slow setup cannot make this flaky.
        let mut seen = false;
        'attempts: for attempt in 0..10 {
            let _ = fs::write(dir.join(format!("probe{attempt}.md")), "p");
            let deadline = Instant::now() + Duration::from_millis(1500);
            while Instant::now() < deadline {
                let _ = tree.poll(Some(&ctx), Instant::now());
                tree.prepare_rows(false);
                if tree.rows().iter().any(|row| row.name.starts_with("probe")) {
                    seen = true;
                    break 'attempts;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert!(
            seen,
            "watcher never reported the new file: {:?}",
            tree.watch_error()
        );
        drop(tree);
        let _ = fs::remove_dir_all(&dir);
    }
}
