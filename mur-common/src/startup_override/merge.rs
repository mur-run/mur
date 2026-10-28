//! Boot step 6: fold an accepted override's flags into the in-memory
//! entitlements. Spec §Boundary is the whole contract — each flag owns the
//! fields listed here and nothing else.
//!
//! The result is never written back to `profile.yaml`; the runtime merges a
//! clone after `pin::verify` and before the seal.

use crate::agent::{
    DEFAULT_DENY_PATHS, Entitlements, NetworkOutboundMode, OverrideFlags, SpawnMode,
};

/// Apply `flags` to `ent` and return one line per field that changed, for the
/// audit's before/after summary. Empty when nothing changed.
///
/// - `net_unrestricted`: `network.outbound.mode = Unrestricted` (B0 rule 2).
///   Per-MCP-server `network` lives on `McpServerEntry`, not here, so MCP
///   servers keep their own mode and stay behind the egress proxy.
/// - `full_perms`: `filesystem.deny` reset to [`DEFAULT_DENY_PATHS`] and
///   `processes.spawn.mode = Any` (B0 rules 1 and 5). A reset, not a removal:
///   `fs.deny` carries no provenance, so user entries cannot be told from
///   template ones. Not a `$HOME` grant — Landlock would drop it whole.
///
/// `no_hitl` has no entitlement field; it is the runtime's `StartupWaiver`.
pub fn apply(ent: &mut Entitlements, flags: OverrideFlags) -> Vec<String> {
    let mut changes = Vec::new();
    if flags.net_unrestricted {
        let before = ent.network.outbound.mode;
        ent.network.outbound.mode = NetworkOutboundMode::Unrestricted;
        if before != NetworkOutboundMode::Unrestricted {
            changes.push(format!(
                "network.outbound.mode: {} -> {}",
                wire(&before),
                wire(&NetworkOutboundMode::Unrestricted)
            ));
        }
    }
    if flags.full_perms {
        let reset: Vec<String> = DEFAULT_DENY_PATHS.iter().map(|p| p.to_string()).collect();
        if ent.filesystem.deny != reset {
            changes.push(format!(
                "filesystem.deny: [{}] -> [{}]",
                ent.filesystem.deny.join(", "),
                reset.join(", ")
            ));
            ent.filesystem.deny = reset;
        }
        let before = ent.processes.spawn.mode;
        ent.processes.spawn.mode = SpawnMode::Any;
        if before != SpawnMode::Any {
            changes.push(format!(
                "processes.spawn.mode: {} -> {}",
                wire(&before),
                wire(&SpawnMode::Any)
            ));
        }
    }
    changes
}

/// The value as `profile.yaml` spells it, so the summary reads like the file.
fn wire<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
