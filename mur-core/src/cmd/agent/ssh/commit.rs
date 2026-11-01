//! Write `profile.yaml`, the entitlement pin and `known_hosts` as one unit.
//!
//! The whole transaction runs under an exclusive per-agent lock, so two
//! `mur agent ssh` commands for one agent never interleave. Without it, two
//! commands could load the same profile and the later save would bring back
//! a grant the earlier one revoked, and the pin would follow (the pin check
//! compares disk to pin, not to the snapshot that was edited).
//!
//! Order under the lock, chosen so that the steps most likely to fail run
//! before anything visible changes:
//!
//! 1. Load the profile and refuse unless the pin matches it (a tampered
//!    profile must be resealed by a person, never laundered by this write).
//! 2. Stage the new `known_hosts` under a fresh name (see [`super::stage`]).
//! 3. Save the profile and advance the pin.
//! 4. Rename the staged `known_hosts` into place.
//!
//! A failure in 3 or 4 restores the profile and pin bytes captured before 3,
//! so the agent is left exactly as it was.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use mur_common::agent::SshEntitlement;
use mur_common::entitlements_pin::{self, PINS_DIR, PinCheck};

use super::super::{load_profile_for_edit, resolve_mur_home, save_profile_pinned};
use super::edit::render_known_hosts;
use super::stage;

/// Suffix of the per-agent lock file in [`PINS_DIR`]. That directory is denied
/// to every agent, so an agent can neither hold the lock nor swap the file.
const LOCK_SUFFIX: &str = "ssh.lock";

/// Bytes of a file before the write, or `None` when it did not exist.
struct Snapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
}

impl Snapshot {
    fn take(path: PathBuf) -> Result<Self> {
        let bytes = match fs::read(&path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Ok(Self { path, bytes })
    }

    fn restore(&self) -> Result<()> {
        match &self.bytes {
            Some(b) => fs::write(&self.path, b),
            None => match fs::remove_file(&self.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        }
        .with_context(|| format!("restore {}", self.path.display()))
    }
}

/// Load `agent`, apply `edit` to its `entitlements.ssh`, and write the result.
pub fn apply<F>(agent: &str, edit: F) -> Result<()>
where
    F: FnOnce(&mut SshEntitlement) -> Result<()>,
{
    let mur_home = resolve_mur_home()?;
    let _lock = lock(&mur_home, agent)?;
    let (profile_path, mut profile) = load_profile_for_edit(agent)?;
    require_trusted_pin(&mur_home, agent, &profile.entitlements)?;

    edit(&mut profile.entitlements.ssh)?;

    let agent_home = profile_path
        .parent()
        .context("profile.yaml has no parent directory")?;
    let staged = stage::stage(agent_home, &render_known_hosts(&profile.entitlements.ssh))?;

    let profile_snap = Snapshot::take(profile_path.clone())?;
    let pin_snap = Snapshot::take(entitlements_pin::pin_path(&mur_home, agent))?;
    let rollback = |why: anyhow::Error| -> anyhow::Error {
        let restored = profile_snap.restore().and_then(|()| pin_snap.restore());
        match restored {
            Ok(()) => why.context("nothing was changed"),
            Err(e) => why.context(format!(
                "and the rollback failed ({e:#}); run `mur agent perm reseal {agent}` after \
                 checking `mur agent ssh list {agent}`"
            )),
        }
    };

    match save_profile_pinned(&profile_path, &mut profile) {
        Ok(true) => {}
        Ok(false) => {
            return Err(rollback(anyhow::anyhow!(
                "the entitlement pin for '{agent}' could not be advanced"
            )));
        }
        Err(e) => return Err(rollback(e)),
    }
    // On error `staged` is already dropped (and removed) by `install`.
    if let Err(e) = staged.install() {
        return Err(rollback(e));
    }
    Ok(())
}

/// Holds `<mur_home>/entitlement-pins/<agent>.ssh.lock`. Dropping it, or the
/// process dying, releases the `flock`. The file is never removed.
struct AgentLock {
    _file: File,
}

/// Take the lock, waiting for another `mur agent ssh` on `agent` to finish.
/// Holders never prompt while holding it (confirmation happens before), so
/// the wait is the length of one write.
fn lock(mur_home: &Path, agent: &str) -> Result<AgentLock> {
    mur_common::agent_name::validate_agent_name(agent)
        .map_err(|e| anyhow::anyhow!("invalid agent name '{agent}': {e}"))?;
    let dir = mur_home.join(PINS_DIR);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = lock_path(mur_home, agent);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    if file.try_lock_exclusive().is_err() {
        eprintln!("waiting for another `mur agent ssh` command on '{agent}' to finish…");
        file.lock_exclusive()
            .with_context(|| format!("lock {}", path.display()))?;
    }
    Ok(AgentLock { _file: file })
}

fn lock_path(mur_home: &Path, agent: &str) -> PathBuf {
    mur_home
        .join(PINS_DIR)
        .join(format!("{agent}.{LOCK_SUFFIX}"))
}

/// The ssh block is only as trustworthy as the pin, so refuse to build on a
/// profile that already differs from it.
fn require_trusted_pin(
    mur_home: &Path,
    agent: &str,
    ent: &mur_common::agent::Entitlements,
) -> Result<()> {
    match entitlements_pin::check(mur_home, agent, ent) {
        Ok(PinCheck::Match | PinCheck::Missing) => Ok(()),
        Ok(PinCheck::Mismatch { changed }) => bail!(
            "'{agent}' has entitlements changed outside MUR ({}); review them with \
             `mur agent perm show {agent}` and run `mur agent perm reseal {agent}` first. \
             Nothing was changed.",
            changed.join(", ")
        ),
        Err(e) => bail!("cannot read the entitlement pin for '{agent}': {e}. Nothing was changed."),
    }
}

/// Run `f` under the same per-agent lock as [`apply`]: for a profile write
/// that is not an ssh edit but must not interleave with one (the `ssh` spawn
/// grant `allow` offers). Never hold it across a prompt.
pub(super) fn with_lock<T>(agent: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _lock = lock(&resolve_mur_home()?, agent)?;
    f()
}

#[cfg(test)]
pub(super) fn hold_lock(mur_home: &Path, agent: &str) -> Result<impl Sized> {
    lock(mur_home, agent)
}
