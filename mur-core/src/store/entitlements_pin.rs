//! Store-level entitlement pin bookkeeping (#712).
//!
//! Lives in `store` so both the CLI profile writers (`cmd::agent`) and the
//! versioned store (`store::versioned::agent::rollback_profile`) share one
//! implementation without the store layer depending on `cmd`.

use std::path::{Path, PathBuf};

/// `(mur_home, agent)` for `<mur_home>/agents/<agent>/profile.yaml`, the only
/// shape that has a pin (and a profile lock). `None` for any other path.
pub fn pin_target(profile_path: &Path) -> Option<(PathBuf, String)> {
    let agent_dir = profile_path.parent()?;
    let agents_root = agent_dir.parent()?;
    let name = agent_dir.file_name()?.to_str()?;
    if agents_root.file_name()?.to_str()? != "agents" {
        return None;
    }
    Some((agents_root.parent()?.to_path_buf(), name.to_string()))
}

/// Take the profile lock for the agent owning `profile_path` (see
/// `mur_common::entitlements_pin::lock_profile`). `None` when the path has no
/// pin, or the name could never be pinned: nothing to serialize against.
pub fn lock_for_profile(
    profile_path: &Path,
) -> anyhow::Result<Option<mur_common::entitlements_pin::ProfileLock>> {
    let Some((mur_home, name)) = pin_target(profile_path) else {
        return Ok(None);
    };
    if mur_common::agent_name::validate_agent_name(&name).is_err() {
        return Ok(None);
    }
    let lock = mur_common::entitlements_pin::lock_profile(&mur_home, &name, || {
        eprintln!("waiting for another command changing '{name}' to finish…");
    })?;
    Ok(Some(lock))
}

/// Refuse a save built on a snapshot another writer has since replaced, when
/// that save would change entitlements.
///
/// Every trusted save stamps a fresh `updated_at`, so a profile whose
/// `updated_at` no longer matches disk was loaded before someone else wrote.
/// Saving it would put back whatever that writer changed, and the pin would
/// follow, since the pin advance trusts what is on disk (#2253 review: a
/// `perm allow-spawn` racing `ssh revoke` restored the revoked grant, pinned).
/// A stale save that leaves entitlements as they are on disk is let through:
/// it cannot widen anything. Call it under the profile lock, so the check and
/// the write cannot be overtaken.
pub fn refuse_stale_entitlements(
    profile_path: &Path,
    on_disk: Option<&mur_common::AgentProfile>,
    new: &mur_common::AgentProfile,
) -> anyhow::Result<()> {
    let Some(disk) = on_disk else {
        return Ok(());
    };
    if disk.updated_at == new.updated_at || disk.entitlements == new.entitlements {
        return Ok(());
    }
    let agent = pin_target(profile_path).map_or_else(|| new.name.clone(), |(_, n)| n);
    anyhow::bail!(
        "'{agent}' was changed by another command after this one read it; nothing was \
         written, so that change is kept. Run this command again."
    )
}

/// Move the #712 entitlement pin to what a trusted writer just saved — only
/// from a trusted state (see `entitlements_pin::advance_pin`). Best-effort: a
/// pin that could not advance makes the next start refuse with a `reseal` hint,
/// which is fail-closed, so the save itself still succeeds.
///
/// Returns whether the pin now matches `new`.
pub fn advance_entitlements_pin(
    profile_path: &Path,
    prior: Option<&mur_common::agent::Entitlements>,
    new: &mur_common::agent::Entitlements,
) -> bool {
    let Some((mur_home, name)) = pin_target(profile_path) else {
        return false;
    };
    let (mur_home, name) = (mur_home.as_path(), name.as_str());
    match mur_common::entitlements_pin::advance_pin(mur_home, name, prior, new) {
        Ok(true) => true,
        Ok(false) => {
            eprintln!(
                "warning: {name}'s entitlements had changed outside MUR before this save; \
             the agent will refuse to start until you review them and run \
             `mur agent perm reseal {name}`"
            );
            false
        }
        Err(e) => {
            tracing::warn!(agent = name, error = %e, "entitlement pin not updated");
            false
        }
    }
}
