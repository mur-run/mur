//! The "repair in progress" marker of `mur channel repair` (#1965).
//!
//! A repair is several durable steps — quarantine the log, move the agents'
//! chat-gate heads, write the router-signed repair record, reseal each of
//! those heads — and a crash (or a missing agent key) can stop it between
//! any two. Without a marker the half-done state can read as *intact*: once
//! the log and head are moved, a manifest-only channel has nothing left to
//! fail a check, so a rerun would skip the unfinished repair and the chat
//! gate would stay blocked for good.
//!
//! So the marker goes down under the append lock *before* anything moves,
//! and comes off only after the last reseal. While it is present the channel
//! reports as damaged (`DamageReport::repair_pending`) and a rerun of repair
//! resumes from it instead of taking the intact shortcut.
//!
//! It lives in `channel-heads/` beside the dirty marker, not in `channels/`,
//! for the same reason: a sandboxed agent can write `channels/` and must not
//! be able to plant or clear it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Extension of a channel's repair marker, beside its `.head`.
const REPAIR_EXT: &str = "repair";

/// What a resumed repair needs to finish exactly as the first run would have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepairMarker {
    /// Quarantine label (a single path component) the first run chose.
    pub label: String,
    /// Who asked for the repair.
    pub by: String,
    /// Facts about the damaged log, carried into the repair record.
    pub before: serde_json::Value,
}

/// `<mur_home>/channel-heads/<id>.repair`.
pub fn marker_path(mur_home: &Path, channel_id: &str) -> Result<PathBuf> {
    Ok(
        crate::head_io::head_path(&crate::anchor::heads_dir(mur_home), channel_id)?
            .with_extension(REPAIR_EXT),
    )
}

/// Write the marker durably (temp + fsync + rename).
pub fn write(mur_home: &Path, channel_id: &str, marker: &RepairMarker) -> Result<()> {
    let path = marker_path(mur_home, channel_id)?;
    fs::create_dir_all(crate::anchor::heads_dir(mur_home))?;
    let tmp = path.with_extension(format!("{REPAIR_EXT}.tmp"));
    let body = serde_json::to_vec(marker).context("serialize repair marker")?;
    let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    f.write_all(&body).context("write repair marker")?;
    f.sync_all().context("fsync repair marker")?;
    fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    #[cfg(unix)]
    fs::File::open(crate::anchor::heads_dir(mur_home))
        .and_then(|d| d.sync_all())
        .context("fsync heads dir after repair marker")?;
    Ok(())
}

/// Read the marker. `Ok(None)` only when there is none; an unreadable one is
/// an error (a repair we cannot resume must not look finished).
pub fn read(mur_home: &Path, channel_id: &str) -> Result<Option<RepairMarker>> {
    let path = marker_path(mur_home, channel_id)?;
    match fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// True while a repair is unfinished. An unsafe id has no marker.
pub fn is_pending(mur_home: &Path, channel_id: &str) -> bool {
    marker_path(mur_home, channel_id).is_ok_and(|p| p.exists())
}

/// Remove the marker once the repair is complete.
pub fn clear(mur_home: &Path, channel_id: &str) -> Result<()> {
    match fs::remove_file(marker_path(mur_home, channel_id)?) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => Ok(r?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn marker_round_trips_and_clears() {
        let tmp = TempDir::new().unwrap();
        assert!(read(tmp.path(), "c1").unwrap().is_none());
        let m = RepairMarker {
            label: "t1".into(),
            by: "me".into(),
            before: serde_json::json!({"old_log_lines": 3}),
        };
        write(tmp.path(), "c1", &m).unwrap();
        assert!(is_pending(tmp.path(), "c1"));
        assert!(tmp.path().join("channel-heads/c1.repair").is_file());
        assert_eq!(read(tmp.path(), "c1").unwrap(), Some(m));
        clear(tmp.path(), "c1").unwrap();
        assert!(!is_pending(tmp.path(), "c1"));
        clear(tmp.path(), "c1").unwrap();
    }

    #[test]
    fn a_garbled_marker_is_an_error_not_absence() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "c1",
            &RepairMarker {
                label: "t".into(),
                by: "b".into(),
                before: serde_json::Value::Null,
            },
        )
        .unwrap();
        fs::write(marker_path(tmp.path(), "c1").unwrap(), b"{").unwrap();
        assert!(read(tmp.path(), "c1").is_err());
        assert!(is_pending(tmp.path(), "c1"));
    }
}
