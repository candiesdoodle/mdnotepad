//! Disk-watcher integration — file watching, reload debouncing, background
//! reads, and the question asked when a file with unsaved edits changes.  Runs per [`Tab`], for every
//! open tab (not just the active one).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use eframe::egui;
use notify::{Event, RecursiveMode, Watcher};

use crate::disk::io::{DiskRevision, disk_revision, read_stable_utf8};
use crate::disk::sync::{DiskChangeChoice, DiskReadMessage, DiskReloadOutcome, PendingDiskChange};
use crate::document::DocumentStats;
use crate::line_ending::EndOfLine;

use crate::tab::Tab;
use crate::{DISK_POLL_INTERVAL, DISK_RELOAD_DEBOUNCE, RustdownApp};

impl RustdownApp {
    /// Drive disk sync for every open tab.  A clean tab silently reloads when
    /// its file changes; a dirty tab's question waits on that tab until the
    /// user activates it.
    pub(crate) fn tick_disk_sync(&mut self, ctx: &egui::Context) {
        for tab in &mut self.tabs {
            tab.tick_disk_sync(ctx);
        }
    }
}

impl Tab {
    fn clear_disk_watcher(&mut self) {
        self.disk.watcher = None;
        self.disk.watch_root = None;
        self.disk.watch_target_name = None;
        self.disk.watch_rx = None;
    }

    pub(crate) fn schedule_disk_reload(&mut self, now: Instant) {
        let due_at = now + DISK_RELOAD_DEBOUNCE;
        if self
            .disk
            .pending_reload_at
            .is_none_or(|existing| existing > due_at)
        {
            self.disk.pending_reload_at = Some(due_at);
        }
    }

    /// Replace the document with the file's text, taking its line endings
    /// as they are, like opening it.
    pub(crate) fn apply_disk_text_state(&mut self, text: Arc<String>, disk_rev: DiskRevision) {
        if let Some(eol) = EndOfLine::detect(text.as_str()) {
            self.doc.line_ending = eol;
        }
        self.doc.text = text.clone();
        self.doc.base_text = text;
        self.doc.disk_rev = Some(disk_rev);
        self.bump_edit_seq();
        self.doc.stats = DocumentStats::from_text(self.doc.text.as_str());
        self.doc.stats_dirty = false;
        self.doc.preview_cache.clear();
        self.doc.preview_dirty = false;
        self.doc.dirty = false;
        self.doc.last_edit_at = None;
        self.doc.editor_galley_cache = None;
        self.error = None;
    }

    /// The file changed while the tab has unsaved edits.  Nothing to do when
    /// it is what the edits started from (a late notice of the tab's own
    /// save); otherwise ask, leaving the text as it is.
    fn note_disk_change(&mut self, disk_text: String, disk_rev: DiskRevision) {
        if disk_text == *self.doc.base_text {
            self.doc.disk_rev = Some(disk_rev);
        } else {
            self.disk.pending_change = Some(PendingDiskChange {
                disk_text,
                disk_rev,
            });
        }
    }

    pub(crate) fn reset_disk_sync_state(&mut self) {
        self.disk.reload_nonce = self.disk.reload_nonce.wrapping_add(1);
        self.disk.poll_at = None;
        self.disk.pending_reload_at = None;
        self.disk.reload_in_flight = false;
        self.disk.pending_change = None;
        self.clear_disk_watcher();
    }

    fn ensure_disk_read_channel(&mut self) {
        if self.disk.read_tx.is_some() {
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.disk.read_tx = Some(tx);
        self.disk.read_rx = Some(rx);
    }

    fn ensure_disk_watcher(&mut self, ctx: &egui::Context, path: &Path) {
        let watch_root = path.parent().unwrap_or_else(|| Path::new("."));
        let target_name = path.file_name().map(ToOwned::to_owned);

        if self.disk.watcher.is_some() && self.disk.watch_root.as_deref() == Some(watch_root) {
            self.disk.watch_target_name = target_name;
            return;
        }

        self.clear_disk_watcher();

        let Some(target_name) = target_name else {
            return;
        };

        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
        let ctx = ctx.clone();
        let handler = move |res| {
            let _ = tx.send(res);
            ctx.request_repaint();
        };

        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(watcher) => watcher,
            Err(err) => {
                self.error
                    .get_or_insert_with(|| format!("Watch setup failed: {err}"));
                return;
            }
        };

        if let Err(err) = watcher.watch(watch_root, RecursiveMode::NonRecursive) {
            self.error
                .get_or_insert_with(|| format!("Watch start failed: {err}"));
            return;
        }

        self.disk.watcher = Some(watcher);
        self.disk.watch_root = Some(watch_root.to_path_buf());
        self.disk.watch_target_name = Some(target_name);
        self.disk.watch_rx = Some(rx);
        self.disk.poll_at = None;
    }

    fn drain_disk_watch_events(&mut self) -> bool {
        let Some(rx) = self.disk.watch_rx.as_ref() else {
            return false;
        };

        let target_name = self.disk.watch_target_name.as_deref();
        let mut saw_change = false;
        let mut watch_error = None;
        let mut disconnected = false;

        loop {
            match rx.try_recv() {
                Ok(Ok(event)) => {
                    if let Some(target) = target_name
                        && event
                            .paths
                            .iter()
                            .any(|path| path.file_name().is_some_and(|name| name == target))
                    {
                        saw_change = true;
                    }
                }
                Ok(Err(err)) => {
                    watch_error = Some(format!("Watch error: {err}"));
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if let Some(err) = watch_error {
            self.error.get_or_insert(err);
            self.clear_disk_watcher();
            return false;
        }
        if disconnected {
            self.clear_disk_watcher();
            return false;
        }
        saw_change
    }

    pub(crate) fn tick_disk_sync(&mut self, ctx: &egui::Context) {
        self.drain_disk_read_results();

        if self.disk.awaits_answer() {
            return;
        }

        let Some(path) = self.doc.path.clone() else {
            self.reset_disk_sync_state();
            return;
        };

        self.ensure_disk_watcher(ctx, path.as_path());

        let now = Instant::now();
        if self.drain_disk_watch_events() {
            self.schedule_disk_reload(now);
        }

        if self.disk.watcher.is_none() && !self.disk.reload_in_flight {
            match self.disk.poll_at {
                Some(next) if now < next => {}
                _ => {
                    self.disk.poll_at = Some(now + DISK_POLL_INTERVAL);

                    match disk_revision(path.as_path()) {
                        Ok(rev) if Some(rev) != self.doc.disk_rev => self.schedule_disk_reload(now),
                        Ok(_) => {}
                        Err(err) => {
                            self.error
                                .get_or_insert_with(|| format!("Disk check failed: {err}"));
                        }
                    }
                }
            }
        } else {
            self.disk.poll_at = None;
        }

        if self.disk.reload_in_flight {
            return;
        }

        if let Some(due_at) = self.disk.pending_reload_at
            && now >= due_at
        {
            self.disk.pending_reload_at = None;
            self.start_disk_reload(ctx, path.clone());
        }

        let mut next_wake = self.disk.pending_reload_at;
        if self.disk.watcher.is_none() {
            next_wake = match (next_wake, self.disk.poll_at) {
                (Some(existing), Some(poll)) => Some(existing.min(poll)),
                (one, other) => one.or(other),
            };
        }

        if let Some(next) = next_wake
            && now < next
        {
            ctx.request_repaint_after(next - now);
        }
    }

    fn start_disk_reload(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.ensure_disk_read_channel();
        let Some(tx) = self.disk.read_tx.clone() else {
            return;
        };

        let edit_seq = self.doc.edit_seq;
        let dirty = self.doc.dirty;

        self.disk.reload_nonce = self.disk.reload_nonce.wrapping_add(1);
        let nonce = self.disk.reload_nonce;

        self.disk.reload_in_flight = true;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let outcome = read_stable_utf8(&path).map(|(disk_text, disk_rev)| {
                if dirty {
                    DiskReloadOutcome::Changed {
                        disk_text,
                        disk_rev,
                    }
                } else {
                    DiskReloadOutcome::Replace {
                        disk_text,
                        disk_rev,
                    }
                }
            });

            let _ = tx.send(DiskReadMessage {
                path,
                nonce,
                edit_seq,
                outcome,
            });
            ctx.request_repaint();
        });
    }

    pub(crate) fn drain_disk_read_results(&mut self) {
        loop {
            let recv = match self.disk.read_rx.as_ref() {
                Some(rx) => rx.try_recv(),
                None => return,
            };
            match recv {
                Ok(msg) => {
                    if msg.nonce != self.disk.reload_nonce {
                        continue;
                    }
                    if self.doc.path.as_deref() != Some(msg.path.as_path()) {
                        continue;
                    }
                    self.disk.reload_in_flight = false;

                    if self.doc.edit_seq != msg.edit_seq {
                        self.schedule_disk_reload(Instant::now());
                        continue;
                    }

                    match msg.outcome {
                        Ok(DiskReloadOutcome::Replace {
                            disk_text,
                            disk_rev,
                        }) => self.apply_disk_text_state(Arc::new(disk_text), disk_rev),
                        Ok(DiskReloadOutcome::Changed {
                            disk_text,
                            disk_rev,
                        }) => self.note_disk_change(disk_text, disk_rev),
                        Err(err) => {
                            self.error = Some(format!("Reload failed: {err}"));
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.disk.reload_in_flight = false;
                    self.disk.read_rx = None;
                    self.disk.read_tx = None;
                    return;
                }
            }
        }
    }

    /// Apply the user's answer to the waiting question (see
    /// [`PendingDiskChange`]).
    pub(crate) fn apply_disk_change_choice(&mut self, choice: DiskChangeChoice) {
        let Some(change) = self.disk.pending_change.take() else {
            return;
        };
        match choice {
            DiskChangeChoice::Ignore => {
                // The file as it is now is what the edits are compared
                // with from here on; the text stays (and stays unsaved).
                self.doc.base_text = Arc::new(change.disk_text);
                self.doc.disk_rev = Some(change.disk_rev);
            }
            DiskChangeChoice::Reload => {
                self.apply_disk_text_state(Arc::new(change.disk_text), change.disk_rev);
            }
            DiskChangeChoice::SaveAs => {
                // As Ignore, so picking this same file replaces it; the
                // question comes back if Save As is cancelled or fails.
                self.doc.base_text = Arc::new(change.disk_text.clone());
                self.doc.disk_rev = Some(change.disk_rev);
                if !self.save_doc(true) {
                    self.disk.pending_change = Some(change);
                }
            }
        }
    }

    /// Take in the file's text read just now (before a save): reload when
    /// there are no unsaved edits, else ask.
    pub(crate) fn incorporate_disk_text(&mut self, disk_text: String, disk_rev: DiskRevision) {
        if self.doc.disk_rev == Some(disk_rev) && disk_text == self.doc.base_text.as_str() {
            return;
        }
        if self.doc.dirty {
            self.note_disk_change(disk_text, disk_rev);
        } else {
            self.apply_disk_text_state(Arc::new(disk_text), disk_rev);
        }
    }
}
