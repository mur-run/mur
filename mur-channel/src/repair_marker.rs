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
//! It lives in `channel-heads/` beside the dirty marker, not in `channels/`,
//! for the same reason: a sandboxed agent can write `channels/` and must not
//! be able to plant or clear it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Extension of a channel's repair marker, beside its `.head`.
const REPAIR_EXT: &str = "repair";

/// `<mur_home>/channel-heads/<id>.repair`.
pub fn marker_path(mur_home: &Path, channel_id: &str) -> Result<PathBuf> {
    Ok(
        crate::head_io::head_path(&crate::anchor::heads_dir(mur_home), channel_id)?
            .with_extension(REPAIR_EXT),
    )
}

/// Put the marker down durably (file and directory fsynced) before the
/// repair touches anything. The body is informational only.
pub fn mark(mur_home: &Path, channel_id: &str, started_at: &str) -> Result<()> {
    let path = marker_path(mur_home, channel_id)?;
    let dir = crate::anchor::heads_dir(mur_home);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let mut f = fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
    f.write_all(started_at.as_bytes())
        .context("write repair marker")?;
    f.sync_all().context("fsync repair marker")?;
    sync_dir(&dir)
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

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("fsync dir {}", dir.display()))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
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
}
