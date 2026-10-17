//! Head file I/O (channel-log-integrity spec 7.3.1 steps 2-4, #1770).
//!
//! The head lives at `<heads_dir>/<channel_id>.head`. [`write_head`] covers the
//! head-side half of the durability order: write a temp file and fsync it,
//! `rename` it over the head, fsync the directory. Step 1 (fsync the log, so
//! the head never points past a line that is not on disk) belongs to the seal
//! hook, which owns the log handle.
//!
//! Callers hold the channel's `events.lock`, so the temp name is fixed: a stale
//! temp left by a crash is simply truncated by the next write.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::head::Head;

/// Extension of a head file.
const HEAD_EXT: &str = "head";
/// Suffix appended to the head file name for the in-flight temp file.
const TMP_SUFFIX: &str = "tmp";

/// `<heads_dir>/<channel_id>.head`. Refuses an id that could leave `heads_dir`
/// or name no file, so a hostile id cannot make the router sign elsewhere.
pub fn head_path(heads_dir: &Path, channel_id: &str) -> Result<PathBuf> {
    crate::id::check_channel_id(channel_id)?;
    Ok(heads_dir.join(format!("{channel_id}.{HEAD_EXT}")))
}

fn tmp_path(head: &Path) -> PathBuf {
    let mut name = head.as_os_str().to_owned();
    name.push(format!(".{TMP_SUFFIX}"));
    PathBuf::from(name)
}

/// Read a channel's head. `Ok(None)` only when there is no head file; a file
/// that exists but cannot be read or parsed is an error, because "no head" and
/// "damaged head" are different verdicts and must not be conflated.
pub fn read_head(heads_dir: &Path, channel_id: &str) -> Result<Option<Head>> {
    let path = head_path(heads_dir, channel_id)?;
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read head {}", path.display())),
    };
    let head =
        serde_json::from_slice(&bytes).with_context(|| format!("parse head {}", path.display()))?;
    Ok(Some(head))
}

/// Durably replace the channel's head with `head`: temp write + fsync, rename,
/// directory fsync. Creates `heads_dir` on first use and syncs its parent so
/// the new directory entry survives a crash too. Any failed step is an error.
pub fn write_head(heads_dir: &Path, head: &Head) -> Result<()> {
    let path = head_path(heads_dir, &head.channel_id)?;
    ensure_dir(heads_dir)?;

    let tmp = tmp_path(&path);
    let body = serde_json::to_vec(head).context("serialize head")?;
    if let Err(e) = write_synced(&tmp, &body) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename head into place {}", path.display()));
    }
    sync_dir(heads_dir).context("fsync heads dir after rename")
}

fn write_synced(tmp: &Path, body: &[u8]) -> Result<()> {
    let mut f = File::create(tmp).with_context(|| format!("create head temp {}", tmp.display()))?;
    f.write_all(body).context("write head temp")?;
    f.sync_all().context("fsync head temp")
}

/// Create `dir` if absent; when it was just created, fsync its parent so the
/// entry itself is durable (spec 7.3.1: the first append syncs the new dir).
fn ensure_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(dir).with_context(|| format!("create heads dir {}", dir.display()))?;
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        sync_dir(parent).context("fsync parent of new heads dir")?;
    }
    Ok(())
}

/// Remove a channel's head and any temp a crashed write left beside it.
/// Idempotent: a missing head (or a missing `heads_dir`) is success, because
/// deleting a channel that was never sealed must not fail. The id is checked
/// before anything is touched.
///
/// The dirty marker is not handled here; it is `anchor`'s file
/// (`anchor::clear_dirty`).
pub fn remove_head(heads_dir: &Path, channel_id: &str) -> Result<()> {
    let path = head_path(heads_dir, channel_id)?;
    let mut removed = false;
    for p in [&path, &tmp_path(&path)] {
        match fs::remove_file(p) {
            Ok(()) => removed = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("remove head {}", p.display())),
        }
    }
    if removed {
        sync_dir(heads_dir)?;
    }
    Ok(())
}

/// fsync a directory so a rename or new entry in it is durable. Windows cannot
/// open a directory as a file; NTFS journals the rename, so this is a no-op
/// there rather than an error.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("fsync dir {}", dir.display()))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}
