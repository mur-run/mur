//! Reload-time gate for hot `mcp_servers` entries.
//!
//! `mur_common`'s classifier decides which entry edits are *eligible* without
//! touching the disk. A changed `command` additionally has to pass, at reload
//! time, the same admission boot runs: the install-time pin (B0 rule 6), the
//! signature check (rule 11), and the seal's exec rule. A refused entry keeps
//! the entry already being served, whole (D9), and the reason is recorded for
//! `mur agent doctor` in `ReloadState::mcp_refusals`.
//!
//! The baseline is the entry last *served*, not the boot entry: after a good
//! reload moved a server to binary B, a later bad edit must leave it on B, not
//! restart it back onto the boot binary.

use crate::hooks::b0::entry::{PinOutcome, verify_entry_pin};
use mur_common::AgentProfile;
use mur_common::agent::McpServerEntry;
use std::collections::BTreeMap;
use std::path::Path;

/// MCP server name → why its edited entry is not the one being served.
pub type Refusals = BTreeMap<String, String>;

/// The entries to serve, and the refusals. `candidate` is the overlay
/// (`base.with_hot_fields_over(served, fresh).mcp_servers`), `served` what the last good
/// reload served. Only an entry whose `command` differs from its served one is
/// checked; `permitted` is the seal's exec predicate.
pub fn gate_entries(
    base: &AgentProfile,
    served: &[McpServerEntry],
    candidate: Vec<McpServerEntry>,
    permitted: &dyn Fn(&Path) -> bool,
) -> (Vec<McpServerEntry>, Refusals) {
    let mut refusals = Refusals::new();
    let out = candidate
        .into_iter()
        .map(|entry| {
            let Some(prev) = served.iter().find(|s| s.name == entry.name) else {
                return entry;
            };
            if prev.command == entry.command {
                return entry;
            }
            match check_launch(&entry, base, permitted) {
                Ok(()) => entry,
                Err(reason) => {
                    tracing::warn!(
                        mcp = %entry.name,
                        reason = %reason,
                        "MCP entry edit refused at reload; still serving the previous entry",
                    );
                    refusals.insert(entry.name.clone(), reason);
                    prev.clone()
                }
            }
        })
        .collect();
    (out, refusals)
}

/// Boot's admission for one entry, as a reload gate: only a positive pin
/// result passes — boot tolerates skips and soft fails because refusing there
/// strands the agent, but a reload that refuses only keeps the old entry.
fn check_launch(
    entry: &McpServerEntry,
    profile: &AgentProfile,
    permitted: &dyn Fn(&Path) -> bool,
) -> Result<(), String> {
    match verify_entry_pin(entry, profile) {
        PinOutcome::Verified => {}
        PinOutcome::Skipped => {
            return Err(format!(
                "new command `{}` has no pin that covers it; restart the agent to apply",
                entry.command
            ));
        }
        PinOutcome::SoftFail(reason) => {
            return Err(format!(
                "pin for `{}` could not be checked: {reason}",
                entry.command
            ));
        }
        PinOutcome::HardFail(refusal) => return Err(refusal),
    }
    // The file the spawn will exec: same resolver and PATH as `mcp_client::spawn`.
    let path = mur_common::exec::resolve_mcp_launch_in(
        &crate::sandbox::search_dirs::mcp_child_path(profile.entitlements.processes.spawn.mode),
        &entry.command,
    )
    .map_err(|e| format!("command `{}` not resolvable: {e}", entry.command))?;
    crate::hooks::b0_helpers::verify_signed(&path)
        .map_err(|e| format!("B0 rule 11: MCP binary signature check failed: {e}"))?;
    if !permitted(&path) {
        return Err(format!(
            "the sandbox does not allow exec of {}; grant it and restart the agent",
            path.display()
        ));
    }
    Ok(())
}

/// The seal's exec rule for `base`'s entitlements, built on first use (most
/// reloads change no command). macOS only: it is the one seal with a
/// predicate that mirrors it; elsewhere a refused exec surfaces as a spawn
/// error in `mcp_status.json` when the new client starts.
pub fn seal_permits(
    base: &AgentProfile,
    agent_home: &Path,
) -> impl Fn(&Path) -> bool + Send + Sync + use<> {
    #[cfg(target_os = "macos")]
    {
        let ent = base.entitlements.clone();
        let home = agent_home.to_path_buf();
        let policy = std::sync::OnceLock::new();
        move |p: &Path| {
            let policy = policy
                .get_or_init(|| crate::sandbox::SandboxPolicy::from_entitlements(&ent, &home));
            let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            crate::sandbox::macos::exec_permitted(policy, &canon)
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (base, agent_home);
        |_: &Path| true
    }
}

#[cfg(test)]
#[path = "live_profile_mcp_tests.rs"]
mod tests;
