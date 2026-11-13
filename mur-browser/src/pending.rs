//! What a session loaded, and the per-session marker that outlives a killed
//! write-back (#2360, spec "Newer-state guard", "Pending write-back marker").
//!
//! [`load`] decrypts a profile under its lock and keeps two things for the
//! session's write-back: the [`Version`] it loaded (the watermark the
//! write-back compares against, so an older session never overwrites a newer
//! login) and a marker file `<profile_dir>/writeback.pending.<pid>-<nanos>`.
//!
//! The marker is held under an OS advisory lock for as long as the session
//! owns it, so "alive" means "its lock is held": the lock dies with the
//! process, and a reused pid cannot keep a dead session's marker alive. This
//! stands in for the spec's pid + start-time check, which has no portable
//! source for the start time. A marker whose lock is free on the next use
//! means the session ended without finishing its write-back (cut short or
//! killed); [`sweep`] reports it once and removes it.
//!
//! Dropping a [`Marker`] removes it: every in-process ending that is not a
//! lost login (a finished write-back, a deliberate skip, a launch that never
//! happened) needs nothing else. Only a write-back that could not finish
//! calls [`Marker::keep`]. A SIGKILL or SIGTERM never runs `Drop`, which is
//! exactly the case the marker is for.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::abandon::{self, ProfileLock};
use crate::paths;
use crate::state::{StateKeyStore, Version, read_versioned};

/// Marker file name prefix inside a profile directory.
pub const MARKER_PREFIX: &str = "writeback.pending.";

/// Marker content of a write-back that gave up waiting for the profile lock.
const LOCK_TIMEOUT_TAG: &[u8] = b"lock_timeout";

/// Why a kept marker's write-back did not finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// The profile lock was not free in time.
    LockTimeout,
    /// The write-back failed or never ran.
    Interrupted,
}

/// One session's live marker on one profile.
#[derive(Debug)]
pub struct Marker {
    path: PathBuf,
    file: Option<File>,
    keep: bool,
}

impl Marker {
    /// Create this session's marker in `dir`, under the profile's held lock.
    fn create(dir: &Path, _held: &ProfileLock) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = dir.join(format!("{MARKER_PREFIX}{}-{nanos}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("create write-back marker {}", path.display()))?;
        file.try_lock_exclusive()
            .with_context(|| format!("lock write-back marker {}", path.display()))?;
        Ok(Self {
            path,
            file: Some(file),
            keep: false,
        })
    }

    /// Leave the marker for the next use to report, tagged with `why`.
    pub fn keep(mut self, why: Kept) {
        if let (Kept::LockTimeout, Some(f)) = (why, self.file.as_mut()) {
            let _ = f.write_all(LOCK_TIMEOUT_TAG);
        }
        self.keep = true;
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            let _ = FileExt::unlock(&f);
        }
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// What one session loaded from one profile.
#[derive(Debug)]
pub struct Loaded {
    pub site: String,
    /// The watermark: the version of the state file this session started from.
    pub version: Version,
    pub marker: Marker,
}

/// Decrypt `site`'s profile for a session, under its lock (bounded by
/// `deadline`): the plaintext, plus the [`Loaded`] its write-back needs.
pub fn load(
    mur_home: &Path,
    site: &str,
    store: &impl StateKeyStore,
    deadline: Instant,
) -> Result<(Loaded, Vec<u8>)> {
    let held = abandon::lock(mur_home, site, deadline)?
        .with_context(|| format!("browser profile {site:?} is busy (another writer holds it)"))?;
    let (plain, version) = read_versioned(&paths::profile_state(mur_home, site), store)?;
    let marker = Marker::create(&paths::profile_dir(mur_home, site), &held)?;
    let loaded = Loaded {
        site: site.to_owned(),
        version,
        marker,
    };
    Ok((loaded, plain))
}

/// Remove every dead marker of `site` (its lock is free), under the held
/// profile lock. Returns what the dead ones lost, `LockTimeout` first. A
/// marker older than the state file's last write is removed silently: a
/// later write superseded whatever it lost.
pub fn sweep(mur_home: &Path, site: &str, _held: &ProfileLock) -> Result<Option<Kept>> {
    let dir = paths::profile_dir(mur_home, site);
    let state_mtime = std::fs::metadata(paths::profile_state(mur_home, site))
        .and_then(|m| m.modified())
        .ok();
    let mut lost = None;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("list {}", dir.display())),
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(MARKER_PREFIX)
        {
            continue;
        }
        let Some(kept) = take_dead(&entry.path(), state_mtime)? else {
            continue;
        };
        lost = match (lost, kept) {
            (Some(Kept::LockTimeout), _) | (_, Some(Kept::LockTimeout)) => Some(Kept::LockTimeout),
            (l, k) => l.or(k),
        };
    }
    Ok(lost)
}

/// `None`: alive, left alone. `Some(None)`: dead but superseded. `Some(Some)`:
/// dead, reporting what it lost. A dead marker is removed either way.
fn take_dead(
    path: &Path,
    state_mtime: Option<std::time::SystemTime>,
) -> Result<Option<Option<Kept>>> {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("open {}", path.display())),
    };
    if file.try_lock_exclusive().is_err() {
        return Ok(None);
    }
    let created = file.metadata().and_then(|m| m.modified()).ok();
    let mut tag = Vec::new();
    let _ = file.read_to_end(&mut tag);
    let _ = FileExt::unlock(&file);
    drop(file);
    std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    let superseded = matches!((state_mtime, created), (Some(s), Some(c)) if s > c);
    let kept = match tag.as_slice() {
        LOCK_TIMEOUT_TAG => Kept::LockTimeout,
        _ => Kept::Interrupted,
    };
    Ok(Some((!superseded).then_some(kept)))
}

/// The one-time notice for a dead marker on `site`.
pub fn notice(site: &str, kept: Kept) -> String {
    match kept {
        Kept::LockTimeout => abandon::Abandoned {
            at: chrono::Utc::now(),
            reason: abandon::Reason::LockTimeout,
            host: None,
            other: None,
        }
        .notice(site),
        Kept::Interrupted => format!(
            "Browser profile {site:?}: the last session's login state may not have been saved; \
             the session ended before its write-back finished. Log in again for this agent if \
             the site asks you to."
        ),
    }
}

#[cfg(test)]
#[path = "pending_tests.rs"]
mod tests;
