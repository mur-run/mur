//! The one writer of an installed agent's `profile.yaml` (spec v3.12/v3.13).
//!
//! Every write goes through a uniquely named temp file created with
//! `O_CREAT|O_EXCL`, synced, then renamed over the target, so a reader never
//! sees half a profile and two writers never share a temp file. An installed
//! agent's writer must hold that agent's [`ProfileLock`] and re-checks under
//! it whether the profile still exists; the type system checks the lock, a
//! source test and review check that nothing else writes the file.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::entitlements_pin::ProfileLock;
use crate::identity_pin::split_agent_home;

/// The profile's file name inside an agent home.
pub const PROFILE_FILE: &str = "profile.yaml";

/// What the writer requires of `profile.yaml` under the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileExpect {
    /// The agent is installed: a missing profile means it was purged while
    /// the caller worked, so the write is skipped, never recreated.
    Exists,
    /// A create path: an existing profile is an error.
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    /// [`ProfileExpect::Exists`] and the profile is gone.
    Skipped,
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileWriteError {
    #[error("profile lock was taken for '{lock}', not '{target}'")]
    WrongLock { lock: String, target: String },
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),
    #[error("{0} is an installed agent home; write it through write_installed_profile")]
    InstalledHome(PathBuf),
    #[error("io error on {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

/// Write `<mur_home>/agents/<agent>/profile.yaml` for the agent `lock` was
/// taken for. Refuses a lock taken for another agent.
pub fn write_installed_profile(
    lock: &ProfileLock,
    agent: &str,
    expect: ProfileExpect,
    bytes: &[u8],
) -> Result<WriteOutcome, ProfileWriteError> {
    if lock.agent() != agent {
        return Err(ProfileWriteError::WrongLock {
            lock: lock.agent().to_string(),
            target: agent.to_string(),
        });
    }
    let dir = lock.mur_home().join(crate::paths::AGENTS).join(agent);
    let target = dir.join(PROFILE_FILE);
    let io_err = |source| ProfileWriteError::Io {
        path: target.clone(),
        source,
    };
    let present = target.is_file();
    match (expect, present) {
        (ProfileExpect::Exists, false) => return Ok(WriteOutcome::Skipped),
        (ProfileExpect::Absent, true) => return Err(ProfileWriteError::AlreadyExists(target)),
        _ => {}
    }
    write_atomic(&dir, bytes).map_err(io_err)?;
    Ok(WriteOutcome::Written)
}

/// Write `<home>/profile.yaml` for a home that is not of the installed shape
/// (an embedded or exported one): no lock, no existence check. Refuses a path
/// that is `<mur_home>/agents/<valid name>`.
pub fn write_other_profile(home: &Path, bytes: &[u8]) -> Result<(), ProfileWriteError> {
    if split_agent_home(home).is_some() {
        return Err(ProfileWriteError::InstalledHome(home.to_path_buf()));
    }
    write_atomic(home, bytes).map_err(|source| ProfileWriteError::Io {
        path: home.join(PROFILE_FILE),
        source,
    })
}

fn write_atomic(dir: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!(".{PROFILE_FILE}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, dir.join(PROFILE_FILE))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests;
