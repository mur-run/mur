//! `mur deep-research provision` — creates restricted worker agents that
//! each mount the `research-gateway` MCP server.
//!
//! Reuses the exact profile-construction path `mur agent create` uses
//! (`cmd::agent::lifecycle::cmd_create`, whose default entitlements already
//! set `network.outbound = restricted` with an empty allow-list) and the
//! exact MCP-attach path `mur agent mcp add` uses
//! (`cmd::agent::mcp::cmd_mcp_add`, which persists through the existing
//! load/save-atomic helpers) — no hand-rolled profile YAML here.
//!
//! Egress for the gateway MCP entry itself is left `None` (Inherit): the
//! per-server `BroadAudited` grant that actually lets the gateway reach the
//! network is a separate, explicit-consent step (Task 8). Provisioning
//! alone must never grant egress.

use std::path::Path;

use anyhow::Result;

use crate::cmd::agent::lifecycle::cmd_create;
use crate::cmd::agent::mcp::{McpAddPin, cmd_mcp_add};

/// Default number of workers `mur deep-research provision` creates when
/// `--count` is omitted.
pub const DEFAULT_WORKER_COUNT: usize = 3;

/// Default agent-name prefix when `--prefix` is omitted.
pub const DEFAULT_WORKER_PREFIX: &str = "dr_worker";

/// Name of the gateway MCP server entry mounted on every worker.
const GATEWAY_MCP_NAME: &str = "research-gateway";

/// Binary invoked for the gateway MCP server (installed on PATH by
/// `build.sh`, shipped by Tasks 1-6).
const GATEWAY_MCP_COMMAND: &str = "mur-research-gateway";

/// Create `count` restricted worker agents named `<name_prefix>_1..N`, each
/// mounting the `research-gateway` MCP server with no egress grant of its
/// own. Returns the created agent names, in order.
pub fn provision(mur_home: &Path, name_prefix: &str, count: usize) -> Result<Vec<String>> {
    // `cmd_create` and `cmd_mcp_add` resolve their home directory via the
    // `MUR_HOME` env var (`resolve_mur_home` / `load_profile_for_edit`), so
    // provisioning against an explicit `mur_home` means pointing that env
    // var at it first — the same pattern `cmd::agent::mcp::tests` uses.
    unsafe {
        std::env::set_var("MUR_HOME", mur_home);
    }

    let mut names = Vec::with_capacity(count);
    for i in 1..=count {
        let name = format!("{name_prefix}_{i}");
        cmd_create(&name, true, None, None, None)?;
        cmd_mcp_add(
            &name,
            GATEWAY_MCP_NAME,
            GATEWAY_MCP_COMMAND,
            &[],
            McpAddPin {
                force: true,
                ..Default::default()
            },
        )?;
        names.push(name);
    }
    Ok(names)
}

/// CLI-facing wrapper for `mur deep-research provision`: applies
/// [`DEFAULT_WORKER_PREFIX`]/[`DEFAULT_WORKER_COUNT`] when the flags are
/// omitted, provisions the workers, and prints their names.
pub fn cmd_provision(
    mur_home: &Path,
    name_prefix: Option<&str>,
    count: Option<usize>,
) -> Result<()> {
    let prefix = name_prefix.unwrap_or(DEFAULT_WORKER_PREFIX);
    let count = count.unwrap_or(DEFAULT_WORKER_COUNT);
    let names = provision(mur_home, prefix, count)?;
    println!("Provisioned {} deep-research worker agent(s):", names.len());
    for name in &names {
        println!("  {name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize tests that mutate the process-wide `MUR_HOME` /
    /// `MUR_AGENT_BIN_DIR` env vars (established pattern, see
    /// `cmd::agent::mcp::tests::MUR_HOME_LOCK`).
    static MUR_HOME_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn provision_creates_restricted_workers_with_gateway() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        // Redirect the runtime-symlink dir cmd_create() also writes into,
        // so the test never touches the developer's real ~/.local/bin.
        let bin_dir = tmp.path().join("bin");
        unsafe {
            std::env::set_var("MUR_AGENT_BIN_DIR", &bin_dir);
        }

        let names = provision(tmp.path(), "dr_worker", 3).unwrap();
        assert_eq!(names.len(), 3);
        assert_eq!(names, vec!["dr_worker_1", "dr_worker_2", "dr_worker_3"]);

        let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();
        assert!(p.mcp_servers.iter().any(|s| s.name == "research-gateway"));
        // Egress NOT granted here — must be Inherit/restricted until the
        // consent step (Task 8).
        let gw = p
            .mcp_servers
            .iter()
            .find(|s| s.name == "research-gateway")
            .unwrap();
        assert!(gw.network.is_none());
        assert_eq!(gw.command, "mur-research-gateway");
        assert!(gw.args.is_empty());

        // Worker holds no egress of its own either.
        assert_eq!(
            p.entitlements.network.outbound.mode,
            mur_common::agent::NetworkOutboundMode::Restricted
        );
        assert!(p.entitlements.network.outbound.allow_hosts.is_empty());
    }
}
