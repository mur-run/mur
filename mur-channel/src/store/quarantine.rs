//! Moving a damaged log aside (#1965), so `mur channel repair` can start a
//! fresh hash chain without deleting the evidence.
//!
//! The quarantined copy stays inside the channel directory under a name no
//! reader treats as the log, so nothing loads, folds, or trusts it again.
//! It runs under the channel's append lock: an append racing the move would
//! otherwise land in the file being moved and vanish from the new chain.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;

use super::{ChannelStore, EVENTS_FILE, LOCK_FILE};

/// Prefix of a quarantined log's file name inside the channel directory.
pub const QUARANTINE_PREFIX: &str = "events.quarantined-";

impl ChannelStore {
    /// Rename the channel's log to `events.quarantined-<ts>.jsonl` beside it,
    /// under the append lock, and run `then` while that lock is still held
    /// (so the caller can reset the signed head before any writer appends to
    /// the fresh log). Returns the quarantined path, or `None` when there was
    /// no log to move. Never deletes anything; refuses to overwrite an
    /// existing quarantine file.
    pub fn quarantine_log<F>(
        &self,
        id: &str,
        now: DateTime<Utc>,
        then: F,
    ) -> Result<Option<PathBuf>>
    where
        F: FnOnce() -> Result<()>,
    {
        let dir = self.channel_dir(id)?;
        if !dir.is_dir() {
            then()?;
            return Ok(None);
        }
        let lock_path = dir.join(LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        lock.lock_exclusive().context("lock channel events")?;
        let out = (|| {
            let log = dir.join(EVENTS_FILE);
            let moved = if log.exists() {
                let name = format!(
                    "{QUARANTINE_PREFIX}{}.jsonl",
                    now.format("%Y%m%dT%H%M%S%.6fZ")
                );
                let dest = dir.join(name);
                if dest.exists() {
                    bail!("quarantine target already exists: {}", dest.display());
                }
                fs::rename(&log, &dest).with_context(|| {
                    format!("quarantine {} -> {}", log.display(), dest.display())
                })?;
                Some(dest)
            } else {
                None
            };
            then()?;
            Ok(moved)
        })();
        FileExt::unlock(&lock).ok();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::{ChannelActor, EventKind};
    use tempfile::TempDir;

    #[test]
    fn the_log_is_moved_aside_not_deleted() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Note,
                serde_json::json!({"n": 1}),
                None,
                None,
                None,
            )
            .unwrap();
        let before = fs::read(store.events_path("c1").unwrap()).unwrap();
        let mut ran = false;
        let moved = store
            .quarantine_log("c1", Utc::now(), || {
                ran = true;
                Ok(())
            })
            .unwrap()
            .expect("there was a log to move");
        assert!(ran);
        assert!(!store.events_path("c1").unwrap().exists());
        assert_eq!(fs::read(&moved).unwrap(), before);
        assert!(
            moved
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(QUARANTINE_PREFIX)
        );
        // The next append starts a fresh log at seq 0.
        let ev = store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Note,
                serde_json::json!({}),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(ev.seq, 0);
    }

    #[test]
    fn a_channel_with_no_log_moves_nothing() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        assert!(
            store
                .quarantine_log("c1", Utc::now(), || Ok(()))
                .unwrap()
                .is_none()
        );
    }
}
