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

/// `rename`, treating a missing source as "nothing to move". A resumed
/// repair can move into a quarantine that already holds a file of that name
/// (something appended after the first move); the newcomer then gets a
/// numbered name rather than overwriting what was kept.
fn move_if_present(from: &Path, to: &Path) -> Result<Option<PathBuf>> {
    if !from.exists() {
        return Ok(None);
    }
    let to = free_name(to);
    let to = to.as_path();
    match fs::rename(from, to) {
        Ok(()) => Ok(Some(to.to_path_buf())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("move {} to {}", from.display(), to.display())),
    }
}

/// `to`, or `<stem>.<n>.<ext>` for the first `n` not yet taken.
fn free_name(to: &Path) -> PathBuf {
    if !to.exists() {
        return to.to_path_buf();
    }
    let stem = to.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = to.extension().and_then(|s| s.to_str());
    (1u32..)
        .map(|n| {
            to.with_file_name(match ext {
                Some(ext) => format!("{stem}.{n}.{ext}"),
                None => format!("{stem}.{n}"),
            })
        })
        .find(|p| !p.exists())
        .expect("an unused name exists")
}

impl ChannelStore {
    /// Quarantine `id`'s log and signed head under `label` (a single path
    /// component, e.g. a timestamp), then clear the dirty marker. Runs under
    /// the channel's append lock, so no append lands mid-move.
    ///
    /// Writes `channel-heads/`, so like `append_and_anchor` it belongs outside
    /// the agent sandbox.
    pub fn quarantine_log(&self, id: &str, label: &str) -> Result<Quarantined> {
        let target = self.channel_dir(id)?.join(QUARANTINE_DIR).join(label);
        if target.exists() {
            anyhow::bail!("quarantine {} already exists", target.display());
        }
        self.quarantine_log_if(id, label, || Ok(Some(())))
            .map(|q| q.expect("unconditional quarantine").0)
    }

    /// [`quarantine_log`](Self::quarantine_log), but only if `still_damaged`
    /// — run under the channel's exclusive append lock, before anything is
    /// moved — returns `Some`. An unlocked check can catch an anchored
    /// append mid-flight (dirty marker down, head one line behind) and call a
    /// healthy channel damaged; by the time this lock is ours that writer has
    /// finished, so the guard sees the settled state. `None` from the guard
    /// = nothing moved and no quarantine dir created. The guard is also the
    /// place to persist anything that must be durable before the move.
    ///
    /// Unlike `quarantine_log`, an existing `label` dir is reused, so an
    /// interrupted repair can resume into the quarantine it started; nothing
    /// already there is overwritten.
    pub fn quarantine_log_if<T>(
        &self,
        id: &str,
        label: &str,
        still_damaged: impl FnOnce() -> Result<Option<T>>,
    ) -> Result<Option<(Quarantined, T)>> {
        crate::id::check_channel_id(label).context("quarantine label")?;
        let dir = self.channel_dir(id)?;
        let head = crate::head_io::head_path(&crate::anchor::heads_dir(&self.mur_home), id)?;
        let target = dir.join(QUARANTINE_DIR).join(label);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

        let _lock = super::lock::AppendLock::exclusive(&dir.join(super::LOCK_FILE))?;
        let Some(guard) = still_damaged()? else {
            return Ok(None);
        };
        fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
        let log = move_if_present(&self.events_path(id)?, &target.join(EVENTS_FILE))?;
        let head = move_if_present(&head, &target.join(QUARANTINED_HEAD_FILE))?;
        // A temp a crashed head write left behind would otherwise be
        // picked up by nothing, but it is not a head for the new chain.
        crate::head_io::remove_head(&crate::anchor::heads_dir(&self.mur_home), id)?;
        crate::anchor::clear_dirty(&self.mur_home, id)?;
        Ok(Some((
            Quarantined {
                dir: target,
                log,
                head,
            },
            guard,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs2::FileExt;
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

    #[test]
    fn a_guard_that_finds_the_channel_settled_moves_nothing() {
        let tmp = TempDir::new().unwrap();
        let store = seeded(&tmp);
        let out = store
            .quarantine_log_if("c1", "t1", || Ok(None::<()>))
            .unwrap();
        assert!(out.is_none());
        assert!(store.events_path("c1").unwrap().exists());
        assert!(tmp.path().join("channel-heads/c1.head").exists());
        assert!(crate::anchor::is_dirty(tmp.path(), "c1"));
        assert!(!tmp.path().join("channels/c1").join(QUARANTINE_DIR).exists());
    }

    #[test]
    fn the_guard_runs_under_the_append_lock() {
        let tmp = TempDir::new().unwrap();
        let store = seeded(&tmp);
        let lock_path = tmp.path().join("channels/c1").join(super::super::LOCK_FILE);
        let (_, held) = store
            .quarantine_log_if("c1", "t1", || {
                // Another thread cannot take the lock while the guard runs.
                let p = lock_path.clone();
                let free = std::thread::spawn(move || {
                    let probe = OpenOptions::new().write(true).open(&p).unwrap();
                    probe.try_lock_exclusive().is_ok()
                })
                .join()
                .unwrap();
                Ok(Some(!free))
            })
            .unwrap()
            .unwrap();
        assert!(held, "guard must run while the append lock is held");
    }

    #[test]
    fn resuming_into_a_quarantine_never_overwrites_what_it_kept() {
        let tmp = TempDir::new().unwrap();
        let store = seeded(&tmp);
        let first = store.quarantine_log("c1", "t1").unwrap();
        // Something landed on the channel before the repair finished.
        store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Note,
                serde_json::json!({"text": "late"}),
                None,
                None,
                None,
            )
            .unwrap();
        let (again, ()) = store
            .quarantine_log_if("c1", "t1", || Ok(Some(())))
            .unwrap()
            .unwrap();
        assert_eq!(again.dir, first.dir);
        let late = again.log.expect("late log moved");
        assert_eq!(late.file_name().unwrap(), "events.1.jsonl");
        assert!(fs::read_to_string(&late).unwrap().contains("late"));
        assert!(
            fs::read_to_string(first.log.unwrap())
                .unwrap()
                .contains("\"x\"")
        );
    }
}
