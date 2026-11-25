//! The live-turn lease of a channel conversation (#2564 review on #2570).
//!
//! An agent runtime answering a channel turn keeps that conversation's
//! history in `<agent_home>/conversations/channels/<id>.json` and writes it
//! when the turn ends. `mur channel delete` must not run under a live turn,
//! or that write lands after the delete and the history is back.
//!
//! Channel `state` cannot say whether a turn is running (every conversation
//! stays `working`), and elapsed time cannot either (a model call or a tool
//! can outlast any window). So the turn says it itself: from admission to
//! its last write it holds a *shared* advisory lock on
//! `<id>.turn` beside the history file. Delete takes the *exclusive* lock,
//! without waiting, on every agent's lease file and holds them all for the
//! whole delete, so:
//! - a turn already running makes delete refuse, however long it has been;
//! - a turn admitted during the delete waits, then finds the channel gone;
//! - a process that died released its lock with it — there is no stale
//!   marker to clean up and no pid to check.
//!
//! Lease files are empty and never unlinked by the runtime (unlinking a lock
//! file races a concurrent opener); delete removes them with the history.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;

/// Under an agent's home: where its runtime keeps channel conversation
/// histories, and their lease files.
pub const AGENT_CHANNEL_HISTORY_DIR: [&str; 2] = ["conversations", "channels"];
/// Extension of a conversation's lease file, beside its `.json` history.
const LEASE_EXT: &str = "turn";

/// `<agent_home>/conversations/channels`.
pub fn agent_history_dir(agent_home: &Path) -> PathBuf {
    AGENT_CHANNEL_HISTORY_DIR
        .iter()
        .fold(agent_home.to_path_buf(), |p, c| p.join(c))
}

/// `<dir>/<channel_id>.turn`. Refuses an id that is not one safe component.
pub fn lease_path(dir: &Path, channel_id: &str) -> Result<PathBuf> {
    crate::id::check_channel_id(channel_id)?;
    Ok(dir.join(format!("{channel_id}.{LEASE_EXT}")))
}

fn open(dir: &Path, channel_id: &str) -> Result<File> {
    let path = lease_path(dir, channel_id)?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open turn lease {}", path.display()))
}

/// Held by a running turn; dropping it ends the lease.
#[derive(Debug)]
pub struct TurnLease {
    _lock: File,
}

/// Take `channel_id`'s lease for one turn, waiting while a delete holds it.
/// Blocking: call off the async workers.
pub fn begin_turn(dir: &Path, channel_id: &str) -> Result<TurnLease> {
    let f = open(dir, channel_id)?;
    FileExt::lock_shared(&f).context("lock turn lease (shared)")?;
    Ok(TurnLease { _lock: f })
}

/// Held by a delete; while it lives no turn can be admitted.
#[derive(Debug)]
pub struct DeleteLease {
    _lock: File,
}

/// The exclusive lease, or `None` when a turn holds it right now. Never
/// waits.
pub fn try_exclusive(dir: &Path, channel_id: &str) -> Result<Option<DeleteLease>> {
    let f = open(dir, channel_id)?;
    match FileExt::try_lock_exclusive(&f) {
        Ok(()) => Ok(Some(DeleteLease { _lock: f })),
        Err(e) if e.kind() == fs2::lock_contended_error().kind() => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context("lock turn lease (exclusive)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01a21066-f6f3-7213-bbae-f7a3eaeeb0f8";

    #[test]
    fn a_running_turn_blocks_the_exclusive_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let turn = begin_turn(tmp.path(), ID).unwrap();
        assert!(try_exclusive(tmp.path(), ID).unwrap().is_none());
        drop(turn);
        assert!(try_exclusive(tmp.path(), ID).unwrap().is_some());
    }

    #[test]
    fn turns_of_several_agents_share_it() {
        let tmp = tempfile::tempdir().unwrap();
        let _a = begin_turn(tmp.path(), ID).unwrap();
        let _b = begin_turn(tmp.path(), ID).unwrap();
        assert!(try_exclusive(tmp.path(), ID).unwrap().is_none());
    }

    #[test]
    fn a_turn_waits_for_a_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let del = try_exclusive(tmp.path(), ID).unwrap().unwrap();
        let dir = tmp.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            let _l = begin_turn(&dir, ID).unwrap();
            tx.send(()).unwrap();
        });
        let waited = rx.recv_timeout(std::time::Duration::from_millis(200));
        assert!(waited.is_err(), "turn admitted while delete held the lease");
        drop(del);
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        t.join().unwrap();
    }

    #[test]
    fn an_unsafe_id_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(begin_turn(tmp.path(), "../x").is_err());
    }
}
