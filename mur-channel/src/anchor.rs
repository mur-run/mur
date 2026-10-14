//! Chain-head anchoring around the signed [`Head`](crate::head::Head) of
//! #1868: where heads live, the dirty marker, and the lag error.
//!
//! A sandboxed agent may append to the log (its grant covers `channels/`) but
//! never to `<mur_home>/channel-heads/`, so a log truncated or rewritten below
//! the signed head is detectable even when every remaining line still chains.
//!
//! Write protocol (see `ChannelService::append_and_anchor`), all under the
//! channel's `events.lock`: `mark_dirty` → append + fsync the log →
//! `head_io::write_head` → `clear_dirty`. The marker goes down *first*: the
//! four fsync steps make the head write itself crash-safe, but nothing makes
//! "log advanced" and "head advanced" one atomic step. A crash between them
//! leaves a log one line past its head, and deleting that line would line
//! the two up again with no trace — except the marker.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use mur_common::channel::ChannelEvent;

/// Directory under `mur_home` that holds one head (and maybe a dirty marker)
/// per channel.
pub const HEADS_DIR: &str = "channel-heads";
/// Extension of a channel's dirty marker, beside its `.head`.
const DIRTY_EXT: &str = "dirty";

/// `<mur_home>/channel-heads/` — the `heads_dir` of [`crate::head_io`].
pub fn heads_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(HEADS_DIR)
}

/// The log line was written but the signed head could not be advanced.
///
/// Callers MUST NOT treat this as "the append failed" — `event` is durable in
/// the log and retrying without its idempotency key would duplicate it. The
/// dirty marker stays, so verification reports the channel until a later
/// anchored append (the same key retries it without a new line) succeeds.
#[derive(Debug)]
pub struct AnchorLagError {
    pub event: ChannelEvent,
    pub source: anyhow::Error,
}

impl std::fmt::Display for AnchorLagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "event seq {} was appended but the signed channel head could not be advanced: {:#}",
            self.event.seq, self.source
        )
    }
}

impl std::error::Error for AnchorLagError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Path of the dirty marker for `channel_id`. Shares `head_path`'s refusal of
/// ids that could leave the directory.
pub fn dirty_path(mur_home: &Path, channel_id: &str) -> Result<PathBuf> {
    Ok(crate::head_io::head_path(&heads_dir(mur_home), channel_id)?.with_extension(DIRTY_EXT))
}

/// Create the dirty marker. Must succeed *before* the log is appended.
pub fn mark_dirty(mur_home: &Path, channel_id: &str) -> Result<()> {
    let path = dirty_path(mur_home, channel_id)?;
    fs::create_dir_all(heads_dir(mur_home))?;
    fs::write(path, b"")?;
    Ok(())
}

/// Remove the dirty marker after a successful head update.
pub fn clear_dirty(mur_home: &Path, channel_id: &str) -> Result<()> {
    match fs::remove_file(dirty_path(mur_home, channel_id)?) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        r => Ok(r?),
    }
}

/// True when an anchored append began and never advanced the head. An unsafe
/// id has no marker by construction, so it is `false`.
pub fn is_dirty(mur_home: &Path, channel_id: &str) -> bool {
    dirty_path(mur_home, channel_id).is_ok_and(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn dirty_marker_lifecycle_beside_the_head() {
        let tmp = TempDir::new().unwrap();
        assert!(!is_dirty(tmp.path(), "c1"));
        mark_dirty(tmp.path(), "c1").unwrap();
        assert!(tmp.path().join("channel-heads/c1.dirty").is_file());
        assert!(is_dirty(tmp.path(), "c1"));
        clear_dirty(tmp.path(), "c1").unwrap();
        assert!(!is_dirty(tmp.path(), "c1"));
        clear_dirty(tmp.path(), "c1").unwrap();
    }

    #[test]
    fn dotted_ids_keep_their_full_name() {
        let tmp = TempDir::new().unwrap();
        let p = dirty_path(tmp.path(), "fleet.dev").unwrap();
        assert_eq!(p.file_name().unwrap(), "fleet.dev.dirty");
    }

    #[test]
    fn unsafe_ids_are_refused() {
        let tmp = TempDir::new().unwrap();
        for bad in ["", "..", "a/b"] {
            assert!(mark_dirty(tmp.path(), bad).is_err(), "{bad:?}");
        }
    }
}
