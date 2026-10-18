//! Spawn-grant check for MCP servers whose launch target is MUR's bundled
//! copy (#1984). Agents installed before the fix carry only the bare name
//! `mur-mcp-server` in `spawn.allowed`; the seal resolves bare names across
//! fixed directories that never include `$MUR_HOME/mcp-servers`, so the
//! restarted agent hits EPERM and gets 0 tools. Doctor names the exact grant.

use std::path::Path;

use mur_common::agent::{McpServerEntry, SpawnEntitlement, SpawnMode};

use super::Check;

/// `None` when the server is not launched from the bundled copy, or the
/// grant already covers it; otherwise a failing check carrying the fix.
pub(super) fn check(
    agent: &str,
    server: &McpServerEntry,
    launch: &Path,
    spawn: &SpawnEntitlement,
) -> Option<Check> {
    if launch != mur_common::exec::bundled_mcp_server_path() {
        return None;
    }
    if !matches!(spawn.mode, SpawnMode::Allowlist | SpawnMode::Strict) {
        return None;
    }
    if is_granted(launch, spawn) {
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

/// Mirrors the seal: an entry containing a path separator is taken literally
/// (no `~` expansion) and compared after canonicalization; `allowed_dirs`
/// grants a whole subtree.
fn is_granted(launch: &Path, spawn: &SpawnEntitlement) -> bool {
    let canon = std::fs::canonicalize(launch).unwrap_or_else(|_| launch.to_path_buf());
    let same = |entry: &str| {
        let p = Path::new(entry);
        p == launch || std::fs::canonicalize(p).is_ok_and(|c| c == canon)
    };
    spawn
        .allowed
        .iter()
        .filter(|e| e.contains(std::path::MAIN_SEPARATOR))
        .any(|e| same(e))
        || spawn
            .allowed_dirs
            .iter()
            .any(|d| std::fs::canonicalize(d).is_ok_and(|dir| canon.starts_with(dir)))
}
