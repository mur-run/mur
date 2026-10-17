//! Move a damaged channel log (and its signed head) aside so the channel can
//! start a fresh hash chain (#1965, `mur channel repair`).
//!
//! Nothing is deleted. The log and head land in
//! `<channel_dir>/quarantine/<label>/`, where `list_ids` never looks (it only
//! lists the top level of `channels/`) and no reader opens them.
//!
//! Order matters for a crash halfway through. The log moves FIRST: a crash
//! after that leaves a head over a missing log, which reads as `Truncated` —
//! still damaged, and a rerun finishes the job. Moving the head first would
//! leave a damaged log with no head, and a log whose only damage was a cut
//! tail would then read as intact: exactly the state the gate must not trust.

use super::*;

/// Directory inside a channel dir that holds quarantined logs.
pub const QUARANTINE_DIR: &str = "quarantine";
/// File name a quarantined signed head is stored under.
pub const QUARANTINED_HEAD_FILE: &str = "channel.head";

/// Where a quarantine put things. `None` = there was nothing to move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantined {
    /// The quarantine directory itself.
    pub dir: PathBuf,
    pub log: Option<PathBuf>,
    pub head: Option<PathBuf>,
}

/// `rename`, treating a missing source as "nothing to move".
fn move_if_present(from: &Path, to: &Path) -> Result<Option<PathBuf>> {
    match fs::rename(from, to) {
        Ok(()) => Ok(Some(to.to_path_buf())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("move {} to {}", from.display(), to.display())),
    }
}

impl ChannelStore {
    /// Quarantine `id`'s log and signed head under `label` (a single path
    /// component, e.g. a timestamp), then clear the dirty marker. Runs under
    /// the channel's append lock, so no append lands mid-move.
    ///
    /// Writes `channel-heads/`, so like `append_and_anchor` it belongs outside
    /// the agent sandbox.
    pub fn quarantine_log(&self, id: &str, label: &str) -> Result<Quarantined> {
        crate::id::check_channel_id(label).context("quarantine label")?;
        let dir = self.channel_dir(id)?;
        let head = crate::head_io::head_path(&crate::anchor::heads_dir(&self.mur_home), id)?;
        let target = dir.join(QUARANTINE_DIR).join(label);
        if target.exists() {
            anyhow::bail!("quarantine {} already exists", target.display());
        }
        fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;

        let lock_path = dir.join("events.lock");
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        lock.lock_exclusive().context("lock channel events")?;
        let moved = (|| -> Result<Quarantined> {
            let log = move_if_present(&self.events_path(id)?, &target.join(EVENTS_FILE))?;
            let head = move_if_present(&head, &target.join(QUARANTINED_HEAD_FILE))?;
            // A temp a crashed head write left behind would otherwise be
            // picked up by nothing, but it is not a head for the new chain.
            crate::head_io::remove_head(&crate::anchor::heads_dir(&self.mur_home), id)?;
            crate::anchor::clear_dirty(&self.mur_home, id)?;
            Ok(Quarantined {
                dir: target.clone(),
                log,
                head,
            })
        })();
        FileExt::unlock(&lock).ok();
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seeded(tmp: &TempDir) -> ChannelStore {
        let store = ChannelStore::new(tmp.path());
        store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Note,
                serde_json::json!({"text": "x"}),
                None,
                None,
                None,
            )
            .unwrap();
        let heads = crate::anchor::heads_dir(tmp.path());
        fs::create_dir_all(&heads).unwrap();
        fs::write(heads.join("c1.head"), b"{}").unwrap();
        crate::anchor::mark_dirty(tmp.path(), "c1").unwrap();
        store
    }

    #[test]
    fn moves_log_and_head_aside_and_clears_the_marker() {
        let tmp = TempDir::new().unwrap();
        let store = seeded(&tmp);
        let q = store.quarantine_log("c1", "t1").unwrap();

        assert!(!store.events_path("c1").unwrap().exists());
        assert!(!tmp.path().join("channel-heads/c1.head").exists());
        assert!(!crate::anchor::is_dirty(tmp.path(), "c1"));
        let log = q.log.expect("log moved");
        assert!(fs::read_to_string(log).unwrap().contains("\"x\""));
        assert_eq!(fs::read(q.head.expect("head moved")).unwrap(), b"{}");
        // The quarantine is not mistaken for a channel.
        assert_eq!(store.list_ids().unwrap(), vec!["c1".to_string()]);
    }

    #[test]
    fn missing_pieces_are_not_an_error_and_labels_never_collide() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        let q = store.quarantine_log("c1", "t1").unwrap();
        assert_eq!((q.log, q.head), (None, None));
        assert!(store.quarantine_log("c1", "t1").is_err(), "reused label");
        assert!(store.quarantine_log("c1", "../x").is_err(), "unsafe label");
    }
}
