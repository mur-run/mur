//! Abandoned write-backs and the one-time notice for them (#2360, spec
//! "Abandoned write-back notice", "Lock").
//!
//! When a session's login state cannot be written back to a profile, that
//! fact is stored in the profile's metadata ([`Abandoned`]) and shown once
//! the next time the profile is used, with the recovery for its reason.
//! Before this, an overlap drop or a cut-short write-back lost a login and
//! left nothing but a log line nobody reads.
//!
//! Every write of a profile's metadata here happens under the profile lock
//! (`<profile_dir>/.lock`, an OS advisory lock, so a SIGKILLed holder
//! releases it). The lock is taken once per operation and the held guard is
//! passed down; nothing below re-locks.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::{auth, paths};

/// Lock file inside a profile directory. Never deleted; it is inert when
/// unlocked.
pub const LOCK_FILE: &str = ".lock";

/// How long an operation outside an agent stop waits for a profile lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(5);

/// How often a waiting operation retries the lock.
const LOCK_RETRY: Duration = Duration::from_millis(25);

/// Why a write-back was not saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Part of the session matched this profile and another one, so it was
    /// written to neither.
    Overlap,
    /// The profile changed since the session loaded it.
    NewerCopy,
    /// The profile lock was not free in time.
    LockTimeout,
}

/// One recorded abandon. Carries host and profile names only, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Abandoned {
    pub at: DateTime<Utc>,
    pub reason: Reason,
    /// For `overlap`: the shared host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// For `overlap`: the other profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
}

impl Abandoned {
    /// The one-time notice for `site`, naming what was lost and the recovery.
    /// No cookie or storage value can reach it: the record holds none.
    pub fn notice(&self, site: &str) -> String {
        match self.reason {
            Reason::Overlap => format!(
                "Browser profile {site:?}: the last session's login for {host} was not saved, \
                 because profile {other:?} covers the same host and one session cannot keep two \
                 logins apart. You may be logged out of {host}. Log in again for this agent, or \
                 bind only one of {site:?} and {other:?} to it.",
                host = self.host.as_deref().unwrap_or("a shared host"),
                other = self.other.as_deref().unwrap_or("another profile"),
            ),
            Reason::NewerCopy => format!(
                "Browser profile {site:?}: the last session's login state was not saved, because \
                 the profile was changed elsewhere while it ran (a newer copy was kept)."
            ),
            Reason::LockTimeout => format!(
                "Browser profile {site:?}: the last session's login state may not have been \
                 saved; the profile was busy when the session ended. Log in again for this agent \
                 if the site asks you to."
            ),
        }
    }
}

/// An exclusive hold on one profile's lock; released on drop.
pub struct ProfileLock(std::fs::File);

impl Drop for ProfileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

/// Take `site`'s profile lock, retrying until `deadline`. `None` on timeout.
pub fn lock(mur_home: &Path, site: &str, deadline: Instant) -> Result<Option<ProfileLock>> {
    lock_dir(&paths::profile_dir(mur_home, site), deadline)
}

/// [`lock`] for a profile known by its directory.
pub fn lock_dir(dir: &Path, deadline: Instant) -> Result<Option<ProfileLock>> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create browser profile dir {}", dir.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))
        .with_context(|| format!("open profile lock in {}", dir.display()))?;
    loop {
        if file.try_lock_exclusive().is_ok() {
            return Ok(Some(ProfileLock(file)));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(LOCK_RETRY.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Record `entry` on `site` under the held lock. A newer record replaces an
/// older one still unshown; one notice per profile is enough to send the
/// user to its recovery.
pub fn record_locked(
    mur_home: &Path,
    site: &str,
    _held: &ProfileLock,
    entry: Abandoned,
) -> Result<()> {
    let path = paths::profile_meta(mur_home, site);
    let mut meta = read_meta(&path)?;
    meta.writeback_abandoned = Some(entry);
    auth::write_meta(&path, &meta)
}

/// [`record_locked`], taking the lock itself (bounded by `deadline`).
pub fn record(mur_home: &Path, site: &str, entry: Abandoned, deadline: Instant) -> Result<bool> {
    match lock(mur_home, site, deadline)? {
        Some(held) => record_locked(mur_home, site, &held, entry).map(|()| true),
        None => Ok(false),
    }
}

/// The pending notice for `site`, if any. Read-only: call [`clear_shown`]
/// once it has been shown.
pub fn pending(mur_home: &Path, site: &str) -> Result<Option<Abandoned>> {
    let path = paths::profile_meta(mur_home, site);
    if !path.is_file() {
        return Ok(None);
    }
    Ok(read_meta(&path)?.writeback_abandoned)
}

/// Clear `shown` from `site` — compare-and-set under the lock: only when the
/// record still equals what was shown, so an abandon recorded while the
/// notice was on screen survives for the next use. `true` when cleared.
pub fn clear_shown(mur_home: &Path, site: &str, shown: &Abandoned) -> Result<bool> {
    let Some(_held) = lock(mur_home, site, Instant::now() + LOCK_WAIT)? else {
        return Ok(false);
    };
    let path = paths::profile_meta(mur_home, site);
    let mut meta = read_meta(&path)?;
    if meta.writeback_abandoned.as_ref() != Some(shown) {
        return Ok(false);
    }
    meta.writeback_abandoned = None;
    auth::write_meta(&path, &meta)?;
    Ok(true)
}

/// Show-once: every pending notice for `sites`, cleared as it is taken —
/// recorded abandons, and dead write-back markers
/// ([`crate::pending::sweep`]) of sessions that never finished.
pub fn take_notices(mur_home: &Path, sites: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for site in sites {
        match sweep_markers(mur_home, site) {
            Ok(Some(kept)) => out.push(crate::pending::notice(site, kept)),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(site, error = %format!("{error:#}"), "could not check a browser profile's unfinished write-backs")
            }
        }
        match pending(mur_home, site) {
            Ok(Some(entry)) => {
                out.push(entry.notice(site));
                if let Err(error) = clear_shown(mur_home, site, &entry) {
                    tracing::warn!(site, error = %format!("{error:#}"), "could not clear a shown browser notice");
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(site, error = %format!("{error:#}"), "could not read a browser profile's notice")
            }
        }
    }
    out
}

/// [`crate::pending::sweep`] under `site`'s lock. A profile that was never
/// saved has no markers and is not created here.
fn sweep_markers(mur_home: &Path, site: &str) -> Result<Option<crate::pending::Kept>> {
    if !paths::profile_dir(mur_home, site).is_dir() {
        return Ok(None);
    }
    let Some(held) = lock(mur_home, site, Instant::now() + LOCK_WAIT)? else {
        return Ok(None);
    };
    crate::pending::sweep(mur_home, site, &held)
}

fn read_meta(path: &Path) -> Result<auth::ProfileMeta> {
    let yaml = std::fs::read_to_string(path)
        .with_context(|| format!("read browser profile metadata {}", path.display()))?;
    serde_yaml::from_str(&yaml)
        .with_context(|| format!("parse browser profile metadata {}", path.display()))
}

#[cfg(test)]
#[path = "abandon_tests.rs"]
mod tests;
