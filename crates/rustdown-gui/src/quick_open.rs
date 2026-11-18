//! File → Quick open (Ctrl+P): type part of a file's name to jump to it.
//! Lists the open tabs' files and the open folder's documents (Markdown
//! and text files; every file with "Show all files"), which a background
//! thread finds, so a big folder never holds up the window.

use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use eframe::egui;

use crate::RustdownApp;

/// Most files listed from a folder (a huge tree stops there).
const MAX_FILES: usize = 20_000;
/// Most matches shown.
const MAX_RESULTS: usize = 50;
/// Folders not searched (besides hidden ones, whose names start with `.`).
const SKIPPED_DIRS: [&str; 3] = ["node_modules", "target", "__pycache__"];
/// Width of the quick open box.
const WIDTH: f32 = 560.0;
/// Height of a result row.
const ROW_HEIGHT: f32 = 26.0;

/// What the quick open box asks the app to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Still open.
    Open,
    /// Closed without choosing.
    Cancelled,
    /// Open this file.
    Chosen(PathBuf),
}

/// The open quick open box.
pub(crate) struct QuickOpen {
    query: String,
    /// Index of the highlighted match.
    selected: usize,
    /// The folder the paths are shown relative to.
    root: Option<PathBuf>,
    /// Every file to match, open tabs' first.
    files: Vec<PathBuf>,
    /// The folder scan, until it has reported.
    scan: Option<mpsc::Receiver<Vec<PathBuf>>>,
}

impl QuickOpen {
    /// Open the box over the open tabs' `tab_files` and, when a folder is
    /// open, the documents under `root` (all files with `show_all`).
    pub(crate) fn new(root: Option<&Path>, show_all: bool, tab_files: Vec<PathBuf>) -> Self {
        let scan = root.map(|root| {
            let (tx, rx) = mpsc::channel();
            let root = root.to_path_buf();
            let _ = thread::Builder::new()
                .name("quick-open-scan".to_owned())
                .spawn(move || {
                    let _ = tx.send(scan_folder(&root, show_all));
                });
            rx
        });
        Self {
            query: String::new(),
            selected: 0,
            root: root.map(Path::to_path_buf),
            files: tab_files,
            scan,
        }
    }

    /// Take the folder scan's files once it is done.
    fn poll_scan(&mut self, ctx: &egui::Context) {
        let Some(scan) = &self.scan else {
            return;
        };
        match scan.try_recv() {
            Ok(found) => {
                for path in found {
                    if !self.files.contains(&path) {
                        self.files.push(path);
                    }
                }
                self.scan = None;
            }
            Err(mpsc::TryRecvError::Empty) => ctx.request_repaint(),
            Err(mpsc::TryRecvError::Disconnected) => self.scan = None,
        }
    }

    /// `path` as shown: relative to the folder when it is inside it.
    fn shown_path<'a>(&self, path: &'a Path) -> std::borrow::Cow<'a, str> {
        self.root
            .as_deref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
            .to_string_lossy()
    }

    /// The files matching the query, best first.
    fn matches(&self) -> Vec<&PathBuf> {
        let mut scored: Vec<(i32, &PathBuf)> = self
            .files
            .iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy();
                score(&self.query, &self.shown_path(path), &name).map(|score| (score, path))
            })
            .collect();
        // Stable: equal scores keep the open tabs first.
        scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, path)| path)
            .collect()
    }

    /// Draw the box; Up / Down pick, Enter opens, Escape or a click
    /// outside closes.
    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Outcome {
        self.poll_scan(ctx);
        let (up, down, enter, escape) = ctx.input_mut(|input| {
            let none = egui::Modifiers::NONE;
            (
                input.consume_key(none, egui::Key::ArrowUp),
                input.consume_key(none, egui::Key::ArrowDown),
                input.consume_key(none, egui::Key::Enter),
                input.consume_key(none, egui::Key::Escape),
            )
        });
        if escape {
            return Outcome::Cancelled;
        }
        let matches: Vec<PathBuf> = self.matches().into_iter().cloned().collect();
        if down {
            self.selected = (self.selected + 1).min(matches.len().saturating_sub(1));
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        self.selected = self.selected.min(matches.len().saturating_sub(1));
        if enter && let Some(path) = matches.get(self.selected) {
            return Outcome::Chosen(path.clone());
        }

        let mut chosen = None;
        let area = egui::Area::new(egui::Id::new("quick_open"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 72.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(WIDTH);
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("Type part of a file name")
                            .desired_width(f32::INFINITY),
                    );
                    edit.request_focus();
                    if edit.changed() {
                        self.selected = 0;
                    }
                    ui.add_space(4.0);
                    if matches.is_empty() {
                        let note = if self.scan.is_some() {
                            "Looking through the folder…"
                        } else if self.files.is_empty() {
                            "Open a folder to find its files here."
                        } else {
                            "No matching files."
                        };
                        ui.weak(note);
                    }
                    for (index, path) in matches.iter().enumerate() {
                        if self.result_row(ui, path, index == self.selected).clicked() {
                            chosen = Some(path.clone());
                        }
                    }
                });
            });
        if let Some(path) = chosen {
            return Outcome::Chosen(path);
        }
        let clicked_outside = ctx.input(|input| {
            input.pointer.any_pressed()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| !area.response.rect.contains(pos))
        });
        if clicked_outside {
            Outcome::Cancelled
        } else {
            Outcome::Open
        }
    }

    /// One match: the file name, then its folder, dimmed.
    fn result_row(&self, ui: &mut egui::Ui, path: &Path, selected: bool) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ROW_HEIGHT),
            egui::Sense::click(),
        );
        let visuals = ui.visuals();
        if selected || response.hovered() {
            let fill = if selected {
                visuals.selection.bg_fill.gamma_multiply(0.3)
            } else {
                visuals.widgets.hovered.weak_bg_fill
            };
            ui.painter().rect_filled(rect, 4.0, fill);
        }
        let name = path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let shown = self.shown_path(path);
        let folder = shown
            .strip_suffix(name.as_str())
            .unwrap_or_default()
            .trim_end_matches(['/', '\\']);
        let font = egui::TextStyle::Body.resolve(ui.style());
        let small = egui::TextStyle::Small.resolve(ui.style());
        let painter = ui.painter();
        let name_rect = painter.text(
            rect.left_center() + egui::vec2(8.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &name,
            font,
            visuals.strong_text_color(),
        );
        painter.text(
            name_rect.right_center() + egui::vec2(10.0, 0.0),
            egui::Align2::LEFT_CENTER,
            folder,
            small,
            visuals.weak_text_color(),
        );
        response
    }
}

impl RustdownApp {
    /// File → Quick open: over the open folder (if any) and the tabs'
    /// files.
    pub(crate) fn open_quick_open(&mut self) {
        let root = self
            .explorer
            .tree
            .as_ref()
            .map(|tree| tree.root().to_path_buf());
        let tab_files = self
            .tabs
            .iter()
            .filter_map(|tab| tab.doc.path.clone())
            .collect();
        self.quick_open = Some(QuickOpen::new(
            root.as_deref(),
            self.explorer.show_all_files,
            tab_files,
        ));
    }

    /// The quick open box while it is open; opens the chosen file.
    pub(crate) fn show_quick_open(&mut self, ctx: &egui::Context) {
        let Some(quick_open) = self.quick_open.as_mut() else {
            return;
        };
        match quick_open.show(ctx) {
            Outcome::Open => {}
            Outcome::Cancelled => {
                self.quick_open = None;
                // Back to typing where the caret was.
                self.focus_editor = true;
            }
            Outcome::Chosen(path) => {
                self.quick_open = None;
                self.open_path(path);
            }
        }
    }
}

/// Whether quick open lists `path`: Markdown and text files, or every
/// file with `show_all`.
fn wanted(path: &Path, show_all: bool) -> bool {
    show_all || crate::is_markdown_path(path) || crate::is_text_path(path)
}

/// The files under `root` that quick open lists (at most [`MAX_FILES`]),
/// sorted; hidden and build folders are skipped, links not followed.
pub(crate) fn scan_folder(root: &Path, show_all: bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_ref()) {
                    folders.push(path);
                }
            } else if kind.is_file() && wanted(&path, show_all) {
                found.push(path);
                if found.len() >= MAX_FILES {
                    found.sort();
                    return found;
                }
            }
        }
    }
    found.sort();
    found
}

/// How well `query` matches a file shown as `path` with file name `name`;
/// `None` when it does not.  Case is ignored.  Best: the name starts with
/// the query, then the name contains it, then the path contains it, then
/// the query's characters appear in order in the path (runs of them in a
/// row count more).  Shorter paths win ties.
pub(crate) fn score(query: &str, path: &str, name: &str) -> Option<i32> {
    let query = query.trim().to_lowercase();
    let path_lower = path.to_lowercase();
    let name_lower = name.to_lowercase();
    let length_penalty = i32::try_from(path_lower.len().min(1000)).unwrap_or(1000) / 8;
    if query.is_empty() {
        return Some(-length_penalty);
    }
    let base = if name_lower.starts_with(&query) {
        3000
    } else if name_lower.contains(&query) {
        2000
    } else if path_lower.contains(&query) {
        1000
    } else {
        // In order, not necessarily together.
        let mut rest = query.chars().peekable();
        let mut run = 0;
        let mut bonus = 0;
        for c in path_lower.chars() {
            if rest.peek() == Some(&c) {
                rest.next();
                run += 1;
                bonus += run;
            } else {
                run = 0;
            }
        }
        if rest.peek().is_some() {
            return None;
        }
        bonus
    };
    Some(base - length_penalty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_beat_paths_and_in_order_letters_still_match() {
        let starts = score("read", "docs/README.md", "README.md");
        let contains = score("me", "docs/README.md", "README.md");
        let in_path = score("docs", "docs/README.md", "README.md");
        let scattered = score("dcrm", "docs/README.md", "README.md");
        assert!(starts > contains && contains > in_path && in_path > scattered);
        assert!(scattered.is_some());
        assert_eq!(score("zzz", "docs/README.md", "README.md"), None);
        // Shorter paths first among equals; an empty query lists all.
        assert!(score("a", "a.md", "a.md") > score("a", "x/y/z/a.md", "a.md"));
        assert!(score("", "a.md", "a.md").is_some());
    }

    #[test]
    fn scan_finds_documents_and_skips_hidden_and_build_folders() {
        let root =
            std::env::temp_dir().join(format!("mdnotepad-quick-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["docs", ".git", "target", "node_modules"] {
            let _ = std::fs::create_dir_all(root.join(dir));
        }
        for file in [
            "a.md",
            "notes.txt",
            "image.png",
            "docs/b.markdown",
            ".git/c.md",
            "target/d.md",
            "node_modules/e.md",
        ] {
            let _ = std::fs::write(root.join(file), "x");
        }
        let names = |show_all| -> Vec<String> {
            scan_folder(&root, show_all)
                .iter()
                .filter_map(|path| path.strip_prefix(&root).ok())
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .collect()
        };
        assert_eq!(names(false), ["a.md", "docs/b.markdown", "notes.txt"]);
        assert_eq!(
            names(true),
            ["a.md", "docs/b.markdown", "image.png", "notes.txt"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
