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
//!
//! One-time, not every boot: a `none` seen after this pass has run is the
//! owner's choice (`mur agent perm set-mode mur processes.spawn none`), not
//! the old template, and must survive every restart and update. The first
//! run that settles the profile — flipped, or already not `none` — leaves a
//! versioned completion marker beside the pin in
//! [`entitlements_pin::PINS_DIR`], which no agent sandbox can write, so the
//! agent cannot delete it to re-arm the migration. A profile skipped for a
//! reseal is not marked: it is still the legacy install, and once resealed
//! the next start repairs it. `set-mode` cannot write `none` meanwhile — a
//! trusted writer does not advance a mismatched pin — so nothing deliberate
//! is lost.
//!
//! Limit: an install that chose `none` on purpose before this pass existed
//! looks exactly like the old template and is flipped once. Setting it again
//! afterwards sticks.

use std::path::{Path, PathBuf};

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

/// Bump when this pass changes meaning, so it runs once more everywhere.
const MIGRATION_ID: &str = "concierge-spawn-mode-v1";

/// `<mur_home>/entitlement-pins/<name>.concierge-spawn-mode-v1.done`: out of
/// every agent's write reach, like the pin it sits beside.
pub fn marker_path(mur_home: &Path, name: &str) -> PathBuf {
    mur_home
        .join(entitlements_pin::PINS_DIR)
        .join(format!("{name}.{MIGRATION_ID}.done"))
}

/// Drop the completion marker with the pin on `mur agent delete --purge`: a
/// concierge seeded again under this name is a fresh install and must get the
/// pass again. No marker is not an error.
pub fn forget(mur_home: &Path, name: &str) -> std::io::Result<()> {
    match std::fs::remove_file(marker_path(mur_home, name)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Flip `<mur_home>/agents/<name>`'s `processes.spawn.mode` from `none` to
/// `allowlist` when `name` is the built-in concierge, once per install.
pub fn migrate_concierge_spawn_mode(mur_home: &Path, name: &str) -> anyhow::Result<Migration> {
    if name != CONCIERGE_AGENT {
        return Ok(Migration::Unchanged);
    }
    let profile_path = mur_home.join("agents").join(name).join("profile.yaml");
    if !profile_path.is_file() {
        // Nothing to judge yet; not marked, so a later seed is still covered.
        return Ok(Migration::Unchanged);
    }
    // Marker read and write under the lock: `mur update` and a booting
    // runtime can race, and only one of them may decide.
    let _lock = entitlements_pin::lock_profile(mur_home, name, || {})?;
    let marker = marker_path(mur_home, name);
    if marker.exists() {
        return Ok(Migration::Unchanged);
    }
    let outcome = migrate_locked(mur_home, name, &profile_path)?;
    if outcome == Migration::SkippedUnpinned {
        return Ok(outcome);
    }
    // Only after the profile and pin agree: a crash before this line reruns
    // the pass, whose resume step finishes the job.
    replace_atomically(&marker, &format!("{}\n", chrono::Utc::now().to_rfc3339()))?;
    Ok(outcome)
}

fn migrate_locked(mur_home: &Path, name: &str, profile_path: &Path) -> anyhow::Result<Migration> {
    let yaml = std::fs::read_to_string(profile_path)?;
    let mut profile: AgentProfile = serde_yaml_ng::from_str(&yaml)?;

    if profile.entitlements.processes.spawn.mode != SpawnMode::None {
        return resume_interrupted(mur_home, name, &profile);
    }
    // Pin compares the unexpanded on-disk form, the same form writers pin.
    let prior = entitlements_pin::entitlements_from_yaml(&yaml, profile_path)?;
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
    replace_atomically(profile_path, &new_yaml)?;
    if let Err(e) = entitlements_pin::write_pin(mur_home, name, &profile.entitlements) {
        // New profile vs old pin would refuse to start: put the old one back.
        // A crash before this point is finished by `resume_interrupted`.
        if let Err(restore) = replace_atomically(profile_path, &yaml) {
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
