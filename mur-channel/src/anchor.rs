//! Chain-head anchors: `<mur_home>/channel-heads/<channel_id>.json` records
//! the `(seq, hash)` of the last line a trusted (non-sandboxed) writer
//! appended. A sandboxed agent may append to the log (its own grant covers
//! `channels/`) but never to this directory, so truncating the log below the
//! anchor is detectable even when every remaining line still chains.
//!
//! Write protocol (see `ChannelService::append_and_anchor`):
//! `mark_dirty` → append log → `write_anchor` → `clear_dirty`. The dirty
//! marker is written *first* so a failure anywhere after it (disk full, I/O
//! error) leaves evidence the anchor is stale; only a successful anchor
//! update removes it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use mur_common::channel::ChannelEvent;
use serde::{Deserialize, Serialize};

/// Directory under `mur_home` that holds one anchor per channel.
pub const ANCHOR_DIR: &str = "channel-heads";

/// The `(seq, hash)` of the last line in a channel log after an append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainTail {
    pub seq: u64,
    pub hash: String,
}

/// The log line was written but the anchor could not be advanced.
///
/// Callers MUST NOT treat this as "the append failed" — `event` is durable
/// in the log and retrying would duplicate it. The channel's dirty marker is
/// left in place so verification reports it until a later anchored append
/// succeeds.
#[derive(Debug)]
pub struct AnchorLagError {
    pub event: ChannelEvent,
    pub tail: ChainTail,
    pub source: io::Error,
}

impl std::fmt::Display for AnchorLagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "event seq {} was appended but the chain-head anchor could not be written: {}",
            self.event.seq, self.source
        )
    }
}

impl std::error::Error for AnchorLagError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn anchor_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(ANCHOR_DIR)
}

/// Path of the anchor file for `channel_id`.
pub fn anchor_path(mur_home: &Path, channel_id: &str) -> PathBuf {
    anchor_dir(mur_home).join(format!("{channel_id}.json"))
}

/// Path of the dirty marker for `channel_id`.
pub fn dirty_path(mur_home: &Path, channel_id: &str) -> PathBuf {
    anchor_dir(mur_home).join(format!("{channel_id}.dirty"))
}

/// Atomically (temp + rename) persist `tail` as the anchor for `channel_id`.
pub fn write_anchor(mur_home: &Path, channel_id: &str, tail: &ChainTail) -> io::Result<()> {
    let dir = anchor_dir(mur_home);
    fs::create_dir_all(&dir)?;
    let final_path = anchor_path(mur_home, channel_id);
    let tmp_path = dir.join(format!(".{channel_id}.json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec(tail).map_err(io::Error::other)?;
    fs::write(&tmp_path, bytes)?;
    if let Err(e) = fs::rename(&tmp_path, &final_path) {
        fs::remove_file(&tmp_path).ok();
        return Err(e);
    }
    Ok(())
}

/// Read the anchor for `channel_id`; `Ok(None)` when none was ever written.
pub fn read_anchor(mur_home: &Path, channel_id: &str) -> io::Result<Option<ChainTail>> {
    match fs::read(anchor_path(mur_home, channel_id)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Create the dirty marker. Must succeed *before* the log is appended.
pub fn mark_dirty(mur_home: &Path, channel_id: &str) -> io::Result<()> {
    fs::create_dir_all(anchor_dir(mur_home))?;
    fs::write(dirty_path(mur_home, channel_id), b"")
}

/// Remove the dirty marker after a successful anchor update.
pub fn clear_dirty(mur_home: &Path, channel_id: &str) -> io::Result<()> {
    match fs::remove_file(dirty_path(mur_home, channel_id)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// True when the last anchored append did not complete.
pub fn is_dirty(mur_home: &Path, channel_id: &str) -> bool {
    dirty_path(mur_home, channel_id).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tail(seq: u64) -> ChainTail {
        ChainTail {
            seq,
            hash: format!("{seq:064x}"),
        }
    }

    #[test]
    fn read_without_anchor_is_none() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read_anchor(tmp.path(), "c1").unwrap(), None);
        assert!(!is_dirty(tmp.path(), "c1"));
    }

    #[test]
    fn round_trip_at_expected_path() {
        let tmp = TempDir::new().unwrap();
        write_anchor(tmp.path(), "c1", &tail(7)).unwrap();
        assert!(tmp.path().join("channel-heads").join("c1.json").is_file());
        assert_eq!(read_anchor(tmp.path(), "c1").unwrap(), Some(tail(7)));
        // No temp file left behind.
        let leftovers: Vec<_> = fs::read_dir(tmp.path().join("channel-heads"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn overwrite_replaces_whole_record() {
        let tmp = TempDir::new().unwrap();
        write_anchor(tmp.path(), "c1", &tail(7)).unwrap();
        write_anchor(tmp.path(), "c1", &tail(8)).unwrap();
        assert_eq!(read_anchor(tmp.path(), "c1").unwrap(), Some(tail(8)));
    }

    #[test]
    fn anchors_are_per_channel() {
        let tmp = TempDir::new().unwrap();
        write_anchor(tmp.path(), "a", &tail(1)).unwrap();
        write_anchor(tmp.path(), "b", &tail(2)).unwrap();
        assert_eq!(read_anchor(tmp.path(), "a").unwrap(), Some(tail(1)));
        assert_eq!(read_anchor(tmp.path(), "b").unwrap(), Some(tail(2)));
    }

    #[test]
    fn corrupt_anchor_is_an_error_not_none() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("channel-heads")).unwrap();
        fs::write(tmp.path().join("channel-heads/c1.json"), b"{nope").unwrap();
        let err = read_anchor(tmp.path(), "c1").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn dirty_marker_lifecycle() {
        let tmp = TempDir::new().unwrap();
        mark_dirty(tmp.path(), "c1").unwrap();
        assert!(is_dirty(tmp.path(), "c1"));
        clear_dirty(tmp.path(), "c1").unwrap();
        assert!(!is_dirty(tmp.path(), "c1"));
        // Clearing twice is fine.
        clear_dirty(tmp.path(), "c1").unwrap();
    }
}
