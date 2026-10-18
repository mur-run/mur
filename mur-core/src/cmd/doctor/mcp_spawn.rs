//! Spawn-grant check for MCP servers whose launch target is MUR's bundled
//! copy (#1984). Agents installed before the fix carry only the bare name
//! `mur-mcp-server` in `spawn.allowed`; the seal resolves bare names across
//! fixed directories that never include `$MUR_HOME/mcp-servers`, so the
//! restarted agent hits EPERM and gets 0 tools. Doctor names the exact grant.

use std::path::Path;

use mur_agent_runtime::sandbox::policy::SandboxPolicy;
use mur_common::agent::{McpServerEntry, SpawnMode};

use super::Check;

/// `None` when the server is not launched from the bundled copy, or the
/// sealed policy already permits it; otherwise a failing check with the fix.
pub(super) fn check(
    agent: &str,
    server: &McpServerEntry,
    launch: &Path,
    policy: &SandboxPolicy,
) -> Option<Check> {
    if launch != mur_common::exec::bundled_mcp_server_path() {
        return None;
    }
    if !matches!(policy.spawn_mode, SpawnMode::Allowlist | SpawnMode::Strict) {
        return None;
    }
    if is_granted(launch, policy) {
        return None;
    }
    let abs = launch.display();
    Some(Check::new(
        format!("mcp_spawn:{}", server.name),
        false,
        format!(
            "the runtime execs the bundled {abs}, but spawn.allowed does not grant that \
             path — the seal never resolves the bare name there, so the server fails \
             with EPERM and the agent gets 0 tools. Fix: \
             `mur agent perm allow-spawn {agent} {abs}`, then restart the agent."
        ),
    ))
}

/// Judged against the EFFECTIVE grants the seal compiles, not the raw
/// profile: `from_entitlements` already expands `~`, drops guarded
/// `allowed_dirs` roots (`/`, home, a bare volume) and missing paths, and
/// derives toolchain prefixes. Re-deriving any of that here is how the
/// check drifted from the seal before.
fn is_granted(launch: &Path, policy: &SandboxPolicy) -> bool {
    let canon = std::fs::canonicalize(launch).unwrap_or_else(|_| launch.to_path_buf());
    [launch, canon.as_path()].iter().any(|p| {
        policy.spawn_allowed_paths.iter().any(|a| a == p)
            || policy
                .spawn_allowed_prefixes
                .iter()
                .any(|d| p.starts_with(d))
    })
}
