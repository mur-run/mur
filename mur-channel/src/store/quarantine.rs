//! Moving a damaged log aside (#1965), so `mur channel repair` can start a
//! fresh hash chain without deleting the evidence.
//!
//! The quarantined copy stays inside the channel directory under a name no
//! reader treats as the log, so nothing loads, folds, or trusts it again.
//! It runs under the channel's append lock, held by the caller across the
//! move AND the fresh chain's first line (`ChannelService::restart_chain`):
//! releasing it in between would let another writer put old, still-valid
//! records into the new log, which the restart would then seal as trusted.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};

use super::{ChannelStore, EVENTS_FILE};

/// Prefix of a quarantined log's file name inside the channel directory.
pub const QUARANTINE_PREFIX: &str = "events.quarantined-";

impl ChannelStore {
    /// Rename the channel's log to `events.quarantined-<ts>.jsonl` beside it.
    /// The caller MUST hold the append lock (`with_append_lock`). Returns the
    /// quarantined path, or `None` when there was no log to move. Never
    /// deletes anything; refuses to overwrite an existing quarantine file.
    pub(crate) fn quarantine_locked(
        &self,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<PathBuf>> {
        let dir = self.channel_dir(id)?;
        let log = dir.join(EVENTS_FILE);
        if !log.exists() {
            return Ok(None);
        }
        let name = format!(
            "{QUARANTINE_PREFIX}{}.jsonl",
            now.format("%Y%m%dT%H%M%S%.6fZ")
        );
        let dest = dir.join(&name);
        if dest.exists() {
            bail!("quarantine target already exists: {}", dest.display());
        }
        // Named in the repair marker before the move, so a rerun after a
        // crash can still reference it (no-op outside a repair).
        if crate::repair_marker::is_pending(&self.mur_home, id) {
            crate::repair_marker::record_quarantine(&self.mur_home, id, &name)?;
        }
        fs::rename(&log, &dest)
            .with_context(|| format!("quarantine {} -> {}", log.display(), dest.display()))?;
        Ok(Some(dest))
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
        let moved = store
            .with_append_lock("c1", || store.quarantine_locked("c1", Utc::now()))
            .unwrap()
            .expect("there was a log to move");
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
                .with_append_lock("c1", || store.quarantine_locked("c1", Utc::now()))
                .unwrap()
                .is_none()
        );
    }
}
