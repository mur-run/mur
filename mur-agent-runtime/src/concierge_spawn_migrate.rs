//! Give the built-in concierge back its shell after an upgrade.
//!
//! The Hub's bundled concierge template shipped `processes.spawn.mode: none`
//! for months. That was harmless while B0 rule 5 was never consulted on the
//! production tool path; v2.98 (#2175) wired the hook in, and from then on
//! every `bash` call of a concierge seeded from that template is refused with
//! `mode=none`. Adding binaries does not help — `none` ignores the allowlist —
//! and no `mur agent perm` verb hinted at the real fix, so users hit a dead end
//! right after `mur update`.
//!
//! This pass flips the concierge, and only the concierge, from `none` to
//! `allowlist` — the mode every newly created agent gets. `allowlist` still
//! exec-gates every binary through the OS sandbox, so nothing beyond what the
//! profile already grants becomes runnable; it just stops the coarse gate from
//! refusing `bash` itself, which murmur cannot work without.
//!
//! Same safety rules as [`crate::mcp_spawn_migrate`]: the profile is changed
//! only when its entitlements match their pin (or there is no pin yet), and
//! the pin is advanced to the result. A profile edited outside MUR is never
//! touched, so this cannot launder a tampered profile into the pin.

use std::path::Path;

use mur_common::agent::{AgentProfile, SpawnMode};
use mur_common::entitlements_pin::{self, PinCheck};
use mur_common::fleet::CONCIERGE_AGENT;

/// Outcome of [`migrate_concierge_spawn_mode`].
#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// Not the concierge, no profile, or its spawn mode is not `none`.
    Unchanged,
    /// `none` → `allowlist`, pin advanced.
    Enabled,
    /// An earlier run wrote the profile but stopped before advancing the pin;
    /// the pin was advanced now. Only when the profile differs from its pin by
    /// exactly this one mode flip.
    Resumed,
    /// Spawn is `none`, but the entitlements on disk differ from their pin;
    /// left for the user to review and reseal.
    SkippedUnpinned,
}

/// Flip `<mur_home>/agents/<name>`'s `processes.spawn.mode` from `none` to
/// `allowlist` when `name` is the built-in concierge.
pub fn migrate_concierge_spawn_mode(mur_home: &Path, name: &str) -> anyhow::Result<Migration> {
    if name != CONCIERGE_AGENT {
        return Ok(Migration::Unchanged);
    }
    let profile_path = mur_home.join("agents").join(name).join("profile.yaml");
    if !profile_path.is_file() {
        return Ok(Migration::Unchanged);
    }
    let _lock = entitlements_pin::lock_profile(mur_home, name, || {})?;
    let yaml = std::fs::read_to_string(&profile_path)?;
    let mut profile: AgentProfile = serde_yaml_ng::from_str(&yaml)?;

    if profile.entitlements.processes.spawn.mode != SpawnMode::None {
        return resume_interrupted(mur_home, name, &profile);
    }
    // Pin compares the unexpanded on-disk form, the same form writers pin.
    let prior = entitlements_pin::entitlements_from_yaml(&yaml, &profile_path)?;
    if matches!(
        entitlements_pin::check(mur_home, name, &prior)?,
        PinCheck::Mismatch { .. }
    ) {
        tracing::warn!(
            agent = name,
            "concierge has processes.spawn.mode: none (its bash tool is refused), but the \
             profile's entitlements changed outside MUR; not migrating. Review and run \
             `mur agent perm reseal {name}`, then `mur agent perm set-mode {name} \
             processes.spawn allowlist`."
        );
        return Ok(Migration::SkippedUnpinned);
    }

    profile.entitlements.processes.spawn.mode = SpawnMode::Allowlist;
    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let new_yaml = serde_yaml_ng::to_string(&profile)?;
    replace_atomically(&profile_path, &new_yaml)?;
    if let Err(e) = entitlements_pin::write_pin(mur_home, name, &profile.entitlements) {
        // New profile vs old pin would refuse to start: put the old one back.
        // A crash before this point is finished by `resume_interrupted`.
        if let Err(restore) = replace_atomically(&profile_path, &yaml) {
            tracing::warn!(agent = name, error = %restore, "could not restore profile after a failed pin write");
        }
        return Err(e.into());
    }
    tracing::info!(
        agent = name,
        "enabled the concierge's shell: processes.spawn.mode none -> allowlist"
    );
    Ok(Migration::Enabled)
}

/// Finish a run that replaced the profile but never advanced the pin. Exact:
/// with the mode set back to `none`, the entitlements must equal the pin.
/// Any other difference still reads as a mismatch and waits for a reseal.
fn resume_interrupted(
    mur_home: &Path,
    name: &str,
    profile: &AgentProfile,
) -> anyhow::Result<Migration> {
    if profile.entitlements.processes.spawn.mode != SpawnMode::Allowlist {
        return Ok(Migration::Unchanged);
    }
    if !matches!(
        entitlements_pin::check(mur_home, name, &profile.entitlements)?,
        PinCheck::Mismatch { .. }
    ) {
        // Match or no pin: nothing interrupted.
        return Ok(Migration::Unchanged);
    }
    let mut before = profile.entitlements.clone();
    before.processes.spawn.mode = SpawnMode::None;
    if entitlements_pin::check(mur_home, name, &before)? != PinCheck::Match {
        return Ok(Migration::Unchanged);
    }
    entitlements_pin::write_pin(mur_home, name, &profile.entitlements)?;
    tracing::info!(
        agent = name,
        "advanced the entitlement pin for an interrupted concierge spawn-mode migration"
    );
    Ok(Migration::Resumed)
}

/// temp + rename: a torn profile.yaml would brick the agent.
fn replace_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents.as_bytes())?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
#[path = "concierge_spawn_migrate_tests.rs"]
mod tests;
