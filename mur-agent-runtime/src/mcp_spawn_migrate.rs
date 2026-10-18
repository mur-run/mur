//! Grant the bundled `mur-mcp-server` by absolute path in profiles written
//! before #1987 (#1984, item 7).
//!
//! The runtime execs MUR's bundled copy under `$MUR_HOME/mcp-servers/`, but
//! installs before #1987 granted only the bare name `mur-mcp-server`. The seal
//! resolves bare `spawn.allowed` names across fixed directories that never
//! include that directory, so those agents hit EPERM on restart and come up
//! with 0 tools. New installs grant both forms (`grant_mcp_spawn`); this pass
//! repairs the old ones so nobody has to run the doctor's remediation by hand.
//!
//! It only ever ADDS the one absolute path the runtime itself launches, and
//! only for an agent that already granted the bare name to an enabled entry —
//! the user consented to running this server, the grant just named the wrong
//! file. Everything else in the profile is left alone.
//!
//! Respects the entitlement pin (#712): the profile is migrated only when the
//! entitlements on disk match their pin (or there is no pin yet), and the pin
//! is advanced to the result. A profile whose entitlements changed outside
//! MUR is never touched, so this cannot launder a tampered profile into the
//! pin — the agent still refuses to start until the user reseals.

use std::path::Path;

use mur_common::agent::{AgentProfile, SpawnMode};
use mur_common::entitlements_pin::{self, PinCheck};

/// Outcome of [`migrate_bundled_spawn_grant`] for one agent.
#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// Nothing to do: no enabled entry launches the bundled server, the spawn
    /// mode does not consult the allowlist, or the path is already granted.
    Unchanged,
    /// The absolute bundled path was added and the pin advanced.
    Granted,
    /// An earlier run wrote the grant but stopped before advancing the pin;
    /// the pin was advanced now. Only when the profile differs from its pin by
    /// exactly that one grant, on a profile that qualified for it.
    Resumed,
    /// A grant was needed, but the entitlements on disk differ from their pin;
    /// left untouched for the user to review and reseal.
    SkippedUnpinned,
}

/// Add `bundled` to `spawn.allowed` of the installed agent at
/// `<mur_home>/agents/<name>` when one of its enabled MCP entries launches
/// the bundled server and only the bare name is granted.
///
/// `bundled` is the runtime's launch target
/// (`mur_common::exec::bundled_mcp_server_path()` in production). It need not
/// exist yet: the supervisor refreshes the copy right before the seal.
pub fn migrate_bundled_spawn_grant(
    mur_home: &Path,
    name: &str,
    bundled: &Path,
) -> anyhow::Result<Migration> {
    let profile_path = mur_home.join("agents").join(name).join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)?;
    let mut profile: AgentProfile = serde_yaml_ng::from_str(&yaml)?;

    if !needs_grant(&profile, bundled) {
        return resume_interrupted(mur_home, name, &profile, bundled);
    }
    // Pin compares the unexpanded on-disk form, the same form writers pin.
    let prior = entitlements_pin::entitlements_from_yaml(&yaml, &profile_path)?;
    if matches!(
        entitlements_pin::check(mur_home, name, &prior)?,
        PinCheck::Mismatch { .. }
    ) {
        tracing::warn!(
            agent = name,
            "bundled mur-mcp-server is granted only by bare name, but the profile's \
             entitlements changed outside MUR; not migrating. Review and run \
             `mur agent perm reseal {name}`, then restart."
        );
        return Ok(Migration::SkippedUnpinned);
    }

    let abs = bundled.to_string_lossy().into_owned();
    profile.entitlements.processes.spawn.allowed.push(abs);
    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let new_yaml = serde_yaml_ng::to_string(&profile)?;
    replace_atomically(&profile_path, &new_yaml)?;
    if let Err(e) = entitlements_pin::write_pin(mur_home, name, &profile.entitlements) {
        // The new profile against the old pin would refuse to start; put the
        // old profile back. If even that fails (or we never get here, e.g. a
        // crash), `resume_interrupted` finishes the job on the next run.
        if let Err(restore) = replace_atomically(&profile_path, &yaml) {
            tracing::warn!(agent = name, error = %restore, "could not restore profile after a failed pin write");
        }
        return Err(e.into());
    }
    tracing::info!(
        agent = name,
        path = %bundled.display(),
        "granted the bundled mur-mcp-server by absolute path (#1984)"
    );
    Ok(Migration::Granted)
}

/// [`migrate_bundled_spawn_grant`] for every installed agent, for `mur
/// update`: a stopped agent and `mur agent doctor` should not report a grant
/// the next start would add anyway. Returns the agents that were migrated;
/// per-agent failures are logged and skipped.
pub fn migrate_bundled_spawn_grant_all(mur_home: &Path, bundled: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(mur_home.join("agents")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("profile.yaml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
        .into_iter()
        .filter(|n| match migrate_bundled_spawn_grant(mur_home, n, bundled) {
            Ok(m) => matches!(m, Migration::Granted | Migration::Resumed),
            Err(e) => {
                tracing::warn!(agent = %n, error = %e, "could not migrate bundled mcp spawn grant");
                false
            }
        })
        .collect()
}

/// Finish a run that replaced the profile but never advanced the pin. Safe
/// only because it is exact: drop the one grant this module appends, and the
/// rest must equal the pin AND qualify for the migration. Any other change —
/// a hand edit, a tampered allowlist — still reads as a mismatch and is left
/// for `mur agent perm reseal`.
fn resume_interrupted(
    mur_home: &Path,
    name: &str,
    profile: &AgentProfile,
    bundled: &Path,
) -> anyhow::Result<Migration> {
    let abs = bundled.to_string_lossy();
    if profile
        .entitlements
        .processes
        .spawn
        .allowed
        .last()
        .map(String::as_str)
        != Some(&*abs)
    {
        return Ok(Migration::Unchanged);
    }
    let mut before = profile.clone();
    before.entitlements.processes.spawn.allowed.pop();
    if !needs_grant(&before, bundled)
        || entitlements_pin::check(mur_home, name, &before.entitlements)? != PinCheck::Match
    {
        return Ok(Migration::Unchanged);
    }
    entitlements_pin::write_pin(mur_home, name, &profile.entitlements)?;
    tracing::info!(
        agent = name,
        "advanced the entitlement pin for an interrupted bundled mur-mcp-server grant (#1984)"
    );
    Ok(Migration::Resumed)
}

/// temp + rename: a torn profile.yaml would brick the agent.
fn replace_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents.as_bytes())?;
    std::fs::rename(&tmp, path)
}

fn needs_grant(profile: &AgentProfile, bundled: &Path) -> bool {
    let spawn = &profile.entitlements.processes.spawn;
    if !matches!(spawn.mode, SpawnMode::Allowlist | SpawnMode::Strict) {
        return false;
    }
    let Some(bare) = bundled.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let abs = bundled.to_string_lossy();
    if spawn.allowed.iter().any(|a| *a == abs) {
        return false;
    }
    // Only where the user already consented to this server by bare name.
    if !spawn.allowed.iter().any(|a| a == bare) {
        return false;
    }
    profile
        .enabled_mcp_servers()
        .iter()
        .any(|e| Path::new(&e.command).file_name() == bundled.file_name())
}

#[cfg(test)]
#[path = "mcp_spawn_migrate_tests.rs"]
mod tests;
