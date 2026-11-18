//! Like Windows 11 Notepad, the window closes without asking about unsaved
//! changes and the next start brings back its tabs, unsaved text included.  The session is kept in
//! `%LOCALAPPDATA%\<app id>\session\`: `session.toml` lists the tabs, and
//! each tab with unsaved text (or an Untitled one) has its text in a
//! `tab-<n>.txt` beside it.  It is written a moment after a change and
//! when the window closes; only the first window does this (a
//! `--new-window` one starts fresh and leaves the session alone).

use std::{
    collections::HashSet,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
};

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::{
    RustdownApp,
    line_ending::EndOfLine,
    md::render::simple_hash,
    tab::{Tab, canonical_path},
};

const SESSION_FILE: &str = "session.toml";
/// How long after a change the session is written (typing goes on).
const SAVE_DELAY_SECONDS: f64 = 2.0;

/// The session file.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct SessionFile {
    /// Index of the active tab in `tabs`.
    active: usize,
    tabs: Vec<SessionTab>,
}

/// One tab of the session.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct SessionTab {
    /// The file, if the tab has one.
    path: Option<String>,
    /// The file (in the session folder) holding the tab's unsaved text.
    unsaved: Option<String>,
    /// Line breaks of new lines: CRLF, else LF.
    crlf: bool,
    /// For a file with unsaved text: a hash of the file's text the edits
    /// were made on, to notice when the file changed meanwhile.
    base_hash: Option<u64>,
}

/// The folder the session is kept in.
fn session_dir() -> Option<PathBuf> {
    Some(crate::app_identity::local_data_dir()?.join("session"))
}

/// Write `tabs` (with `active` the active one's index) as the session in
/// `dir`, and remove text files of tabs no longer there.
fn write_session(dir: &Path, tabs: &[Tab], active: usize) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut session = SessionFile::default();
    let mut kept = HashSet::new();
    for (index, tab) in tabs.iter().enumerate() {
        let doc = &tab.doc;
        // Nothing to bring back from an empty Untitled tab.
        if doc.path.is_none() && doc.text.is_empty() {
            continue;
        }
        if index == active {
            session.active = session.tabs.len();
        }
        let unsaved = (doc.dirty || doc.path.is_none()).then(|| format!("tab-{}.txt", tab.id));
        if let Some(name) = &unsaved {
            fs::write(dir.join(name), doc.text.as_bytes())?;
            kept.insert(name.clone());
        }
        session.tabs.push(SessionTab {
            path: doc
                .path
                .as_deref()
                .map(|path| path.to_string_lossy().into_owned()),
            base_hash: (doc.dirty && doc.path.is_some()).then(|| simple_hash(&doc.base_text)),
            unsaved,
            crlf: doc.line_ending == EndOfLine::CrLf,
        });
    }
    let text = toml::to_string_pretty(&session).map_err(std::io::Error::other)?;
    // Whole or not at all: a new file, then renamed over the old one.
    let file = dir.join(SESSION_FILE);
    let temporary = dir.join(format!("{SESSION_FILE}.new"));
    fs::write(&temporary, text)?;
    fs::rename(&temporary, &file)?;
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let tab_text = name.starts_with("tab-")
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"));
        if tab_text && !kept.contains(&name) {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

/// The tabs of the session in `dir` (none when there is no session), and
/// the index of the one that was active.
fn read_session(dir: &Path) -> (Vec<Tab>, usize) {
    let Ok(text) = fs::read_to_string(dir.join(SESSION_FILE)) else {
        return (Vec::new(), 0);
    };
    let session: SessionFile = toml::from_str(&text).unwrap_or_default();
    let mut tabs = Vec::new();
    let mut active = 0;
    for (index, entry) in session.tabs.into_iter().enumerate() {
        let unsaved = entry
            .unsaved
            .as_deref()
            .and_then(|name| fs::read_to_string(dir.join(name)).ok());
        let Some(tab) = restore_tab(entry, unsaved) else {
            continue;
        };
        if index == session.active {
            active = tabs.len();
        }
        tabs.push(tab);
    }
    (tabs, active)
}

/// One tab of the session: its file as it is now with the unsaved text
/// on top, or an Untitled tab with its text.  `None` when nothing is left
/// to show (an unreadable file, or a deleted one without unsaved text).
fn restore_tab(entry: SessionTab, unsaved: Option<String>) -> Option<Tab> {
    let line_ending = if entry.crlf {
        EndOfLine::CrLf
    } else {
        EndOfLine::Lf
    };
    let Some(path) = entry.path.map(PathBuf::from) else {
        let text = unsaved?;
        let mut tab = Tab::default();
        tab.doc.line_ending = line_ending;
        tab.init_document(None, text, None);
        // Never saved: closing the tab asks first.
        tab.note_text_changed(false);
        return Some(tab);
    };
    if unsaved.is_none() && !path.exists() {
        return None;
    }
    let mut tab = Tab::from_path(path).ok()?;
    if let Some(text) = unsaved
        && text != *tab.doc.text
    {
        let changed_on_disk = entry
            .base_hash
            .is_some_and(|hash| hash != simple_hash(&tab.doc.base_text));
        tab.doc.text = Arc::new(text);
        tab.doc.edit_seq = tab.doc.edit_seq.wrapping_add(1);
        tab.note_text_changed(false);
        if changed_on_disk {
            tab.error = Some(
                "This file changed on disk since these unsaved edits were made. \
                 Check it before saving."
                    .to_owned(),
            );
        }
    }
    Some(tab)
}

impl RustdownApp {
    /// Whether this window keeps the session: the first window does (not
    /// in tests).
    pub(crate) const fn keeps_session(&self) -> bool {
        self.windows.is_primary() && !cfg!(test)
    }

    /// At startup: bring back the session's tabs before the ones this
    /// launch opened (whose active tab stays active); an empty Untitled
    /// start tab gives way to them.
    pub(crate) fn reopen_session(&mut self) {
        if let Some(dir) = session_dir() {
            self.restore_session_from(&dir);
        }
    }

    fn restore_session_from(&mut self, dir: &Path) {
        let (restored, session_active) = read_session(dir);
        let mut restored: Vec<Tab> = restored
            .into_iter()
            .filter(|tab| {
                tab.doc.path.as_deref().is_none_or(|path| {
                    let canonical = canonical_path(path);
                    !self.tabs.iter().any(|open| open.has_path(&canonical))
                })
            })
            .collect();
        if restored.is_empty() {
            return;
        }
        let launched = self
            .tabs
            .iter()
            .any(|tab| !tab.is_untouched())
            .then(|| self.tab().id);
        self.tabs.retain(|tab| !tab.is_untouched());
        let count = restored.len();
        restored.append(&mut self.tabs);
        self.tabs = restored;
        self.active = launched
            .and_then(|id| self.tab_index(id))
            .unwrap_or_else(|| session_active.min(count - 1));
        self.on_active_tab_changed();
    }

    /// Write the session now, when this window keeps it.
    pub(crate) fn save_session(&mut self) {
        if !self.keeps_session() {
            return;
        }
        if let Some(dir) = session_dir() {
            let _ = write_session(&dir, &self.tabs, self.active);
        }
        self.session_signature = self.current_session_signature();
        self.session_changed_at = None;
    }

    /// What the session depends on: the tabs, their files and edits, and
    /// the active one.
    fn current_session_signature(&self) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        self.active.hash(&mut hasher);
        for tab in &self.tabs {
            (tab.id, &tab.doc.path, tab.doc.edit_seq, tab.doc.dirty).hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Once per frame: write the session [`SAVE_DELAY_SECONDS`] after it
    /// last changed.
    pub(crate) fn tick_session(&mut self, ctx: &egui::Context) {
        if !self.keeps_session() {
            return;
        }
        let now = ctx.input(|input| input.time);
        if self.current_session_signature() != self.session_signature {
            let since = *self.session_changed_at.get_or_insert(now);
            if now - since >= SAVE_DELAY_SECONDS {
                self.save_session();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(SAVE_DELAY_SECONDS));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mdnotepad-session-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn untitled(text: &str) -> Tab {
        let mut tab = Tab::default();
        tab.init_document(None, text.to_owned(), None);
        tab
    }

    #[test]
    fn session_brings_back_tabs_and_unsaved_text() {
        let dir = temp_dir("roundtrip");
        let file = dir.join("notes.md");
        let _ = fs::write(&file, "on disk\n");
        let mut edited = Tab::from_path(file.clone()).unwrap_or_default();
        edited.doc.text = Arc::new("on disk\nand typed\n".to_owned());
        edited.note_text_changed(false);
        let tabs = vec![untitled(""), untitled("draft"), edited];
        let session = dir.join("session");
        assert!(write_session(&session, &tabs, 2).is_ok());

        // A fresh start: the empty Untitled tab gives way to the session.
        let mut app = RustdownApp::default();
        app.restore_session_from(&session);
        let texts: Vec<&str> = app.tabs.iter().map(|tab| tab.doc.text.as_str()).collect();
        assert_eq!(texts, ["draft", "on disk\nand typed\n"]);
        assert!(app.tabs.iter().all(|tab| tab.doc.dirty), "unsaved again");
        assert_eq!(app.tabs[1].doc.base_text.as_str(), "on disk\n");
        assert!(app.tabs[1].error.is_none());
        assert_eq!(app.active, 1, "the active tab is active again");

        // The file changed meanwhile: the edits come back with a warning.
        let _ = fs::write(&file, "changed elsewhere\n");
        let mut app = RustdownApp::default();
        app.restore_session_from(&session);
        assert!(app.tabs[1].error.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launched_file_stays_active_and_is_not_restored_twice() {
        let dir = temp_dir("launch");
        let file = dir.join("a.md");
        let _ = fs::write(&file, "a\n");
        let tabs = vec![
            untitled("draft"),
            Tab::from_path(file.clone()).unwrap_or_default(),
        ];
        let session = dir.join("session");
        assert!(write_session(&session, &tabs, 0).is_ok());

        let mut app = RustdownApp::default();
        app.open_path(file);
        app.restore_session_from(&session);
        assert_eq!(app.tabs.len(), 2, "a.md once");
        assert_eq!(
            app.tab().doc.text.as_str(),
            "a\n",
            "the launched file is active"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleted_files_without_edits_and_stale_text_files_go() {
        let dir = temp_dir("stale");
        let session = dir.join("session");
        let _ = fs::create_dir_all(&session);
        let _ = fs::write(session.join("tab-999.txt"), "old");
        let gone = SessionTab {
            path: Some(dir.join("gone.md").to_string_lossy().into_owned()),
            ..SessionTab::default()
        };
        assert!(restore_tab(gone, None).is_none());
        assert!(write_session(&session, &[untitled("x")], 0).is_ok());
        assert!(!session.join("tab-999.txt").exists(), "stale text removed");
        let _ = fs::remove_dir_all(&dir);
    }
}
