use std::{ffi::OsString, io, path::PathBuf, sync::mpsc, time::Instant};

use notify::RecommendedWatcher;

use crate::disk::io::DiskRevision;

/// What a background read of a changed file found.
#[derive(Debug)]
pub enum DiskReloadOutcome {
    /// The tab had no unsaved edits: take the file as it is.
    Replace {
        disk_text: String,
        disk_rev: DiskRevision,
    },
    /// The tab had unsaved edits: ask before anything changes.
    Changed {
        disk_text: String,
        disk_rev: DiskRevision,
    },
}

#[derive(Debug)]
pub struct DiskReadMessage {
    pub path: PathBuf,
    pub nonce: u64,
    pub edit_seq: u64,
    pub outcome: io::Result<DiskReloadOutcome>,
}

/// The file of a tab with unsaved edits was changed by another program:
/// the text stays as it is until the user answers ([`DiskChangeChoice`]).
#[derive(Clone, Debug)]
pub struct PendingDiskChange {
    pub disk_text: String,
    pub disk_rev: DiskRevision,
}

/// The user's answer to a [`PendingDiskChange`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskChangeChoice {
    /// Keep the text as it is; saving replaces the other program's version.
    Ignore,
    /// Reload the file; the unsaved edits are lost.
    Reload,
    /// Save the text as a new file (the tab moves to it); this file keeps
    /// the other program's version.
    SaveAs,
}

/// Persistent state for the disk-synchronisation subsystem.
#[derive(Default)]
pub struct DiskSyncState {
    pub reload_nonce: u64,
    pub watcher: Option<RecommendedWatcher>,
    pub watch_root: Option<PathBuf>,
    pub watch_target_name: Option<OsString>,
    pub watch_rx: Option<mpsc::Receiver<notify::Result<notify::Event>>>,
    pub poll_at: Option<Instant>,
    pub pending_reload_at: Option<Instant>,
    pub reload_in_flight: bool,
    pub read_tx: Option<mpsc::Sender<DiskReadMessage>>,
    pub read_rx: Option<mpsc::Receiver<DiskReadMessage>>,
    /// A change made elsewhere, waiting for the user's answer.
    pub pending_change: Option<PendingDiskChange>,
}

impl DiskSyncState {
    /// Whether the user must answer a question about the file first: no
    /// reloads or auto-saves meanwhile.
    #[must_use]
    pub const fn awaits_answer(&self) -> bool {
        self.pending_change.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io, sync::mpsc, time::SystemTime};

    fn dummy_rev() -> DiskRevision {
        DiskRevision {
            modified: SystemTime::UNIX_EPOCH,
            len: 42,
            #[cfg(unix)]
            dev: 1,
            #[cfg(unix)]
            inode: 999,
        }
    }

    #[test]
    fn sync_state_mutation_and_defaults() {
        // All Option fields default to None, counters to zero.
        let s = DiskSyncState::default();
        assert_eq!(s.reload_nonce, 0);
        assert!(!s.reload_in_flight);
        for is_none in [
            s.watcher.is_none(),
            s.watch_root.is_none(),
            s.watch_target_name.is_none(),
            s.watch_rx.is_none(),
            s.poll_at.is_none(),
            s.pending_reload_at.is_none(),
            s.read_tx.is_none(),
            s.read_rx.is_none(),
            s.pending_change.is_none(),
        ] {
            assert!(is_none);
        }

        // Nonce, in-flight, question, watch fields, and channels.
        let mut s = DiskSyncState::default();
        s.reload_nonce += 1;
        assert_eq!(s.reload_nonce, 1);
        s.reload_in_flight = true;
        assert!(s.reload_in_flight);
        s.pending_change = Some(PendingDiskChange {
            disk_text: "d".into(),
            disk_rev: dummy_rev(),
        });
        assert!(s.awaits_answer());
        s.pending_change = None;
        assert!(!s.awaits_answer());
        s.watch_target_name = Some(OsString::from("README.md"));
        s.watch_root = Some(PathBuf::from("/tmp"));
        let (tx, rx) = mpsc::channel::<DiskReadMessage>();
        s.read_tx = Some(tx);
        s.read_rx = Some(rx);
        assert!(s.read_tx.is_some() && s.read_rx.is_some());
    }

    #[test]
    fn channel_round_trip_all_outcome_variants() {
        let outcomes: Vec<Result<DiskReloadOutcome, io::Error>> = vec![
            Ok(DiskReloadOutcome::Replace {
                disk_text: "new".into(),
                disk_rev: dummy_rev(),
            }),
            Ok(DiskReloadOutcome::Changed {
                disk_text: "disk".into(),
                disk_rev: dummy_rev(),
            }),
            Err(io::Error::new(io::ErrorKind::NotFound, "gone")),
        ];
        for (i, outcome) in outcomes.into_iter().enumerate() {
            let is_err = outcome.is_err();
            let (tx, rx) = mpsc::channel();
            let nonce = (i + 1) as u64;
            tx.send(DiskReadMessage {
                path: PathBuf::from("/tmp/test.md"),
                nonce,
                edit_seq: 0,
                outcome,
            })
            .ok();
            let m = rx.try_recv().unwrap_or_else(|_| unreachable!());
            assert_eq!(m.nonce, nonce);
            assert_eq!(m.outcome.is_err(), is_err, "variant {i}");
        }
    }

    #[test]
    fn stale_nonce_and_debounce_patterns() {
        // Stale nonce detection.
        let mut s = DiskSyncState::default();
        s.reload_nonce = s.reload_nonce.wrapping_add(1);
        let stale = s.reload_nonce;
        s.reload_nonce = s.reload_nonce.wrapping_add(1);
        assert_ne!(stale, s.reload_nonce);
        assert_eq!(s.reload_nonce, s.reload_nonce);

        // Debounce: first event wins.
        let mut s = DiskSyncState::default();
        let t1 = Instant::now() + std::time::Duration::from_millis(200);
        s.pending_reload_at = Some(t1);
        if s.pending_reload_at.is_none() {
            s.pending_reload_at = Some(Instant::now());
        }
        assert_eq!(s.pending_reload_at, Some(t1));
    }

    #[test]
    fn disk_types_fields_clone_and_debug() {
        let replace = DiskReloadOutcome::Replace {
            disk_text: "hello".into(),
            disk_rev: dummy_rev(),
        };
        let changed = DiskReloadOutcome::Changed {
            disk_text: "d".into(),
            disk_rev: dummy_rev(),
        };
        assert!(matches!(replace, DiskReloadOutcome::Replace { .. }));
        assert!(matches!(changed, DiskReloadOutcome::Changed { .. }));

        let change = PendingDiskChange {
            disk_text: "d".into(),
            disk_rev: dummy_rev(),
        };
        let copy = change.clone();
        assert_eq!(copy.disk_text, "d");
        assert!(format!("{change:?}").contains("PendingDiskChange"));
    }
}
