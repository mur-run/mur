//! One per-agent lock for every writer that advances the entitlement pin.
//!
//! The pin advance trusts the entitlements on disk at save time, so a writer
//! that loaded the profile, then lost a race to another writer, would save its
//! stale copy and move the pin onto it: a revoked grant comes back, pinned.
//! Writers that advance the pin take this lock around their check-and-write
//! (and `mur agent ssh` around its whole load–edit–save), so the check that
//! the profile is still the one they loaded cannot be overtaken.
//!
//! The file lives in [`super::PINS_DIR`], which no agent sandbox may write, so
//! an agent can neither hold the lock nor swap the file. It is never removed;
//! dropping the guard, or the process dying, releases the `flock`.
//!
//! Not reentrant: `flock` is per open file, so a holder that takes it again in
//! the same process waits on itself. Code that already holds it passes the
//! guard down instead.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fs2::FileExt;

use super::{PINS_DIR, PinError, checked_name};
use crate::lock_file::LOCK_POLL;

/// Suffix of the lock file beside `<agent>.json` in [`PINS_DIR`].
const LOCK_SUFFIX: &str = "profile.lock";

/// Held while a writer checks and writes `agent`'s profile and pin.
#[derive(Debug)]
pub struct ProfileLock {
    _file: File,
    mur_home: PathBuf,
    agent: String,
}

impl ProfileLock {
    /// The agent this lock was taken for. A writer refuses a lock taken for
    /// another name.
    pub fn agent(&self) -> &str {
        &self.agent
    }

    pub fn mur_home(&self) -> &Path {
        &self.mur_home
    }
}

/// `<mur_home>/entitlement-pins/<agent>.profile.lock`.
pub fn lock_path(mur_home: &Path, agent: &str) -> PathBuf {
    mur_home
        .join(PINS_DIR)
        .join(format!("{agent}.{LOCK_SUFFIX}"))
}

/// Take `agent`'s profile lock, waiting for the current holder. `on_wait` runs
/// once, before blocking, only when another writer holds it (so a CLI can say
/// why it paused). Holders never prompt, so the wait is one write long.
pub fn lock_profile(
    mur_home: &Path,
    agent: &str,
    on_wait: impl FnOnce(),
) -> Result<ProfileLock, PinError> {
    checked_name(agent)?;
    let path = lock_path(mur_home, agent);
    let io_err = |source: io::Error| PinError::Io {
        path: path.clone(),
        source,
    };
    std::fs::create_dir_all(mur_home.join(PINS_DIR)).map_err(io_err)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(io_err)?;
    if file.try_lock_exclusive().is_err() {
        on_wait();
        file.lock_exclusive().map_err(io_err)?;
    }
    Ok(ProfileLock {
        _file: file,
        mur_home: mur_home.to_path_buf(),
        agent: agent.to_string(),
    })
}

/// Take `agent's profile lock, polling a non-blocking `flock` until
/// `deadline` (one monotonic deadline for the whole wait, like the lifecycle
/// lock). A caller that holds the lifecycle lock passes that lock's deadline.
pub fn lock_profile_until(
    mur_home: &Path,
    agent: &str,
    deadline: Instant,
) -> Result<ProfileLock, PinError> {
    checked_name(agent)?;
    let path = lock_path(mur_home, agent);
    let io_err = |source: io::Error| PinError::Io {
        path: path.clone(),
        source,
    };
    std::fs::create_dir_all(mur_home.join(PINS_DIR)).map_err(io_err)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(io_err)?;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => {
                return Ok(ProfileLock {
                    _file: file,
                    mur_home: mur_home.to_path_buf(),
                    agent: agent.to_string(),
                });
            }
            Err(e) if e.kind() == fs2::lock_contended_error().kind() => {}
            Err(e) => return Err(io_err(e)),
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(PinError::Timeout { path });
        }
        std::thread::sleep(LOCK_POLL.min(deadline - now));
    }
}

#[cfg(test)]
mod tests;
