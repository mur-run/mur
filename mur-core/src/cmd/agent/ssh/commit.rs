//! Write `profile.yaml`, the entitlement pin and `known_hosts` as one unit.
//!
//! Order, chosen so that the steps most likely to fail run before anything
//! visible changes:
//!
//! 1. Refuse unless the pin matches the profile on disk now (a tampered
//!    profile must be resealed by a person, never laundered by this write).
//! 2. Stage the new `known_hosts` next to the real one.
//! 3. Save the profile and advance the pin.
//! 4. Rename the staged `known_hosts` into place.
//!
//! A failure in 3 or 4 restores the profile and pin bytes captured before 3,
//! so the agent is left exactly as it was.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::agent::{SshEntitlement, known_hosts_path};
use mur_common::entitlements_pin::{self, PinCheck};

use super::super::{load_profile_for_edit, resolve_mur_home, save_profile_pinned};
use super::edit::render_known_hosts;

/// Suffix of the staged `known_hosts` while a write is in flight.
const STAGED_SUFFIX: &str = "mur-staged";

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
    let (profile_path, mut profile) = load_profile_for_edit(agent)?;
    require_trusted_pin(&mur_home, agent, &profile.entitlements)?;

    edit(&mut profile.entitlements.ssh)?;

    let agent_home = profile_path
        .parent()
        .context("profile.yaml has no parent directory")?;
    let kh_path = known_hosts_path(agent_home);
    let staged = stage_known_hosts(&kh_path, &render_known_hosts(&profile.entitlements.ssh))?;

    let profile_snap = Snapshot::take(profile_path.clone())?;
    let pin_snap = Snapshot::take(entitlements_pin::pin_path(&mur_home, agent))?;
    let rollback = |why: anyhow::Error| -> anyhow::Error {
        let _ = fs::remove_file(&staged);
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
    if let Err(e) =
        fs::rename(&staged, &kh_path).with_context(|| format!("install {}", kh_path.display()))
    {
        return Err(rollback(e));
    }
    Ok(())
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

/// Write `content` beside `kh_path` and return the staged path.
fn stage_known_hosts(kh_path: &Path, content: &str) -> Result<PathBuf> {
    let dir = kh_path.parent().context("known_hosts path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let staged = kh_path.with_extension(STAGED_SUFFIX);
    fs::write(&staged, content).with_context(|| format!("write {}", staged.display()))?;
    Ok(staged)
}
