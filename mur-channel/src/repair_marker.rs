//! The "repair in progress" marker of `mur channel repair` (#1965).
//!
//! The append lock keeps other writers out of a repair; it does not survive
//! the repair's own process dying. A crash after the log is moved aside and
//! the old heads dropped, but before the fresh chain's signed first record
//! lands, leaves an empty (or missing) log and no head — which reads as a
//! brand-new, *intact* channel. Writers would then build on a chain with no
//! record of the repair, and a rerun of repair would take the intact
//! shortcut and never finish it.
//!
//! So the marker goes down, durably, under the append lock *before* anything
//! moves, and comes off only after the fresh chain's first record and head
//! are written. While it is present the channel reports as damaged
//! (`DamageReport::repair_pending`), every gate refuses it, and a rerun of
//! repair redoes the restart from the top.
//!
//! The marker also carries the names of the logs this repair moved aside,
//! each recorded *before* its rename. A rerun finds no log left to move, so
//! without this the finished repair record could not name the damaged
//! history it quarantined (#1965: "referenced by the repair event").
//!
//! It lives in `channel-heads/` beside the dirty marker, not in `channels/`,
//! for the same reason: a sandboxed agent can write `channels/` and must not
//! be able to plant or clear it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::head_io::{ensure_dir, sync_dir};

/// Extension of a channel's repair marker, beside its `.head`.
const REPAIR_EXT: &str = "repair";
/// Suffix of the in-flight temp file a marker rewrite renames over it.
const TMP_SUFFIX: &str = "tmp";

/// What the marker records. Unreadable bodies count as empty: presence of
/// the file is what refuses the channel; the body only feeds the audit
/// record.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Body {
    started_at: String,
    /// File names (not paths) inside the channel directory, oldest first.
    #[serde(default)]
    quarantined: Vec<String>,
}

/// `<mur_home>/channel-heads/<id>.repair`.
pub fn marker_path(mur_home: &Path, channel_id: &str) -> Result<PathBuf> {
    Ok(
        crate::head_io::head_path(&crate::anchor::heads_dir(mur_home), channel_id)?
            .with_extension(REPAIR_EXT),
    )
}

/// Put the marker down durably before the repair touches anything: the file,
/// its directory, and — when `channel-heads/` is new (a Gate B channel's
/// first repair) — the directory's own entry in `mur_home`.
///
/// A marker already down belongs to an interrupted repair being resumed; it
/// is kept as is, so the logs that run moved aside stay referenced.
pub fn mark(mur_home: &Path, channel_id: &str, started_at: &str) -> Result<()> {
    if is_pending(mur_home, channel_id) {
        return Ok(());
    }
    write(
        mur_home,
        channel_id,
        &Body {
            started_at: started_at.to_owned(),
            quarantined: Vec::new(),
        },
    )
}

/// Record, durably, that `file_name` is about to be (or was) moved aside.
/// Call it *before* the rename: a name recorded for a move that never
/// happened is filtered out by the reader; a move that is not recorded is
/// lost from the audit record.
pub fn record_quarantine(mur_home: &Path, channel_id: &str, file_name: &str) -> Result<()> {
    let mut body = read(mur_home, channel_id);
    if !body.quarantined.iter().any(|n| n == file_name) {
        body.quarantined.push(file_name.to_owned());
    }
    write(mur_home, channel_id, &body)
}

/// The logs this repair moved aside, oldest first (the original damaged log
/// leads). Empty when there is no marker.
pub fn quarantined(mur_home: &Path, channel_id: &str) -> Vec<String> {
    read(mur_home, channel_id).quarantined
}

/// True while a repair is unfinished. An unsafe id has no marker by
/// construction, so it is `false`.
pub fn is_pending(mur_home: &Path, channel_id: &str) -> bool {
    marker_path(mur_home, channel_id).is_ok_and(|p| p.exists())
}

/// Take the marker off once the fresh chain is written and sealed.
pub fn clear(mur_home: &Path, channel_id: &str) -> Result<()> {
    match fs::remove_file(marker_path(mur_home, channel_id)?) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => {
            r.context("remove repair marker")?;
            sync_dir(&crate::anchor::heads_dir(mur_home))
        }
    }
}

fn read(mur_home: &Path, channel_id: &str) -> Body {
    marker_path(mur_home, channel_id)
        .ok()
        .and_then(|p| fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// temp + fsync → rename → fsync dir, so a crash leaves either the old
/// marker or the new one, never a torn or missing one.
fn write(mur_home: &Path, channel_id: &str, body: &Body) -> Result<()> {
    let path = marker_path(mur_home, channel_id)?;
    ensure_dir(&crate::anchor::heads_dir(mur_home))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{TMP_SUFFIX}"));
    let tmp = PathBuf::from(tmp);
    let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    f.write_all(&serde_json::to_vec(body)?)
        .context("write repair marker")?;
    f.sync_all().context("fsync repair marker")?;
    fs::rename(&tmp, &path).with_context(|| format!("install {}", path.display()))?;
    sync_dir(&crate::anchor::heads_dir(mur_home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn marker_goes_down_and_comes_off() {
        let tmp = TempDir::new().unwrap();
        assert!(!is_pending(tmp.path(), "c1"));
        mark(tmp.path(), "c1", "t0").unwrap();
        assert!(is_pending(tmp.path(), "c1"));
        assert!(tmp.path().join("channel-heads/c1.repair").is_file());
        clear(tmp.path(), "c1").unwrap();
        assert!(!is_pending(tmp.path(), "c1"));
        // Clearing twice is fine.
        clear(tmp.path(), "c1").unwrap();
    }

    #[test]
    fn an_unsafe_id_has_no_marker() {
        let tmp = TempDir::new().unwrap();
        assert!(mark(tmp.path(), "../x", "t").is_err());
        assert!(!is_pending(tmp.path(), "../x"));
    }

    /// Review P2 on #1977: the quarantined names survive a resumed repair,
    /// because re-marking keeps the existing body.
    #[test]
    fn quarantined_names_survive_a_remark() {
        let tmp = TempDir::new().unwrap();
        mark(tmp.path(), "c1", "t0").unwrap();
        record_quarantine(tmp.path(), "c1", "events.quarantined-a.jsonl").unwrap();
        mark(tmp.path(), "c1", "t1").unwrap();
        record_quarantine(tmp.path(), "c1", "events.quarantined-b.jsonl").unwrap();
        record_quarantine(tmp.path(), "c1", "events.quarantined-b.jsonl").unwrap();
        assert_eq!(
            quarantined(tmp.path(), "c1"),
            ["events.quarantined-a.jsonl", "events.quarantined-b.jsonl"]
        );
        clear(tmp.path(), "c1").unwrap();
        assert!(quarantined(tmp.path(), "c1").is_empty());
    }

    /// A body that is not ours still refuses the channel; it just names no
    /// quarantined log.
    #[test]
    fn a_garbled_body_is_still_pending() {
        let tmp = TempDir::new().unwrap();
        let p = marker_path(tmp.path(), "c1").unwrap();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"not json").unwrap();
        assert!(is_pending(tmp.path(), "c1"));
        assert!(quarantined(tmp.path(), "c1").is_empty());
    }
}
