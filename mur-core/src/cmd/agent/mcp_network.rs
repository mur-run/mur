//! `mur agent mcp set-network` — per-server outbound egress policy.
//!
//! Moved verbatim out of `mcp.rs` (800-line rule). No behaviour change:
//! policy construction, the BroadAudited grant, and its audit event.

use anyhow::{Context, Result, bail};
use mur_common::agent::{EgressAuthorization, McpNetMode, McpServerNetwork};
use mur_common::telemetry::METHOD_EGRESS_BROAD_AUDITED_ENABLED;

use super::{load_profile_for_edit, resolve_mur_home, save_profile};

/// Pure mapping from CLI args to a per-server network policy:
/// `off` ⇒ `Off`; a non-empty allowlist ⇒ `Restricted`; empty + !off ⇒ clear
/// to `None` (inherit the agent-level policy). Non-`BroadAudited` modes never
/// carry an `authorization` record.
fn network_policy_from_args(allow_hosts: Vec<String>, off: bool) -> Option<McpServerNetwork> {
    if off {
        Some(McpServerNetwork {
            mode: McpNetMode::Off,
            allow_hosts: vec![],
            deny_hosts: vec![],
            authorization: None,
        })
    } else if allow_hosts.is_empty() {
        None
    } else {
        Some(McpServerNetwork {
            mode: McpNetMode::Restricted,
            allow_hosts,
            deny_hosts: vec![],
            authorization: None,
        })
    }
}

/// Pure builder for a `BroadAudited` grant: allow-ALL except `deny_hosts`,
/// stamped with who authorized it and when. Requires explicit operator
/// consent upstream (`cmd_mcp_set_network` prompts unless `yes`).
fn broad_audited_network(
    deny_hosts: Vec<String>,
    authorized_by: String,
    authorized_at_ms: u64,
) -> McpServerNetwork {
    McpServerNetwork {
        mode: McpNetMode::BroadAudited,
        allow_hosts: vec![],
        deny_hosts,
        authorization: Some(EgressAuthorization {
            authorized_by,
            authorized_at_ms,
        }),
    }
}

/// Append a `mur.egress.broad_audited.enabled` event to today's trace log,
/// mirroring `skill_curate::record_curation`.
fn record_broad_audited_enabled(
    mur_home: &std::path::Path,
    agent: &str,
    server_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let traces_dir = mur_home.join("traces");
    std::fs::create_dir_all(&traces_dir)
        .with_context(|| format!("create {}", traces_dir.display()))?;
    let path = traces_dir
        .join(now.format("%Y-%m-%d").to_string())
        .with_extension("jsonl");

    let line = serde_json::json!({
        "ts": now.to_rfc3339(),
        "method": METHOD_EGRESS_BROAD_AUDITED_ENABLED,
        "mur.agent.name": agent,
        "mur.mcp.server_id": server_id,
    });

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

/// Set (or clear) a per-server egress policy on `server_id`. Restart the agent
/// to apply (the sandbox + proxy are wired at supervisor startup).
///
/// `broad_audited=true` requests an allow-ALL-except-`deny_hosts` grant,
/// routed through the audited egress proxy — a permission-required action.
/// Unless `yes` is set, prompts for explicit `[y/N]` consent on stdin before
/// writing the authorization record and emitting telemetry.
pub fn cmd_mcp_set_network(
    agent: &str,
    server_id: &str,
    allow_hosts: Vec<String>,
    deny_hosts: Vec<String>,
    off: bool,
    broad_audited: bool,
    yes: bool,
) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    let srv = profile
        .mcp_servers
        .iter_mut()
        .find(|s| s.name == server_id)
        .ok_or_else(|| anyhow::anyhow!("MCP server '{server_id}' not found on '{agent}'"))?;

    if broad_audited {
        println!(
            "This grants MCP server '{server_id}' on agent '{agent}' outbound access to ALL hosts \
             except: {}\nEvery CONNECT is audited, but this is a broad trust grant — only use it \
             for tools that legitimately need unenumerable destinations (e.g. a research browser).",
            if deny_hosts.is_empty() {
                "(none)".to_string()
            } else {
                deny_hosts.join(", ")
            }
        );
        if !yes {
            print!("\nApprove broad-audited egress? [y/N] ");
            use std::io::{self, Write};
            io::stdout().flush().ok();
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .with_context(|| "read confirmation from stdin")?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                bail!("broad-audited egress grant cancelled");
            }
        }

        let authorized_by = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
        let authorized_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        srv.network = Some(broad_audited_network(
            deny_hosts,
            authorized_by,
            authorized_at_ms,
        ));
        save_profile(&path, &mut profile)?;

        let mur_home = resolve_mur_home()?;
        record_broad_audited_enabled(&mur_home, agent, server_id, chrono::Utc::now())?;
    } else {
        // Any non-BroadAudited mode change clears a prior authorization.
        srv.network = network_policy_from_args(allow_hosts, off);
        save_profile(&path, &mut profile)?;
    }

    // Name the resulting state, not just "updated". Clearing the policy is the
    // case that misleads: it reads as "inherit the agent's allow_hosts", and it
    // is not — that list is enforced in-process and a spawned server never runs
    // the code that enforces it.
    let srv = profile
        .mcp_servers
        .iter()
        .find(|s| s.name == server_id)
        .expect("server was just edited");
    match srv.network.as_ref().map(|n| n.mode).unwrap_or_default() {
        McpNetMode::Inherit => println!(
            "Cleared the egress policy for '{server_id}': it is now bounded only by the OS \
             sandbox, which restricts PORTS, not hosts — the agent's `allow_hosts` does NOT \
             apply to it. To bound it by host: mur agent mcp set-network {agent} {server_id} \
             --allow-host <host>"
        ),
        McpNetMode::Off => println!("'{server_id}' now has no outbound access."),
        _ => println!("Updated egress policy for '{server_id}'."),
    }
    println!("Restart the agent to apply.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::MUR_HOME_LOCK;
    use super::*;

    #[test]
    fn network_policy_from_args_maps_modes() {
        // Empty + !off → clear: no per-server policy, so the server is bounded
        // by the OS sandbox (ports) alone — NOT by the agent's allow_hosts.
        assert_eq!(network_policy_from_args(vec![], false), None);
        // off → Off, regardless of hosts.
        assert_eq!(
            network_policy_from_args(vec![], true).unwrap().mode,
            McpNetMode::Off
        );
        // Non-empty allowlist → Restricted with those hosts.
        let r = network_policy_from_args(vec!["example.com".into()], false).unwrap();
        assert_eq!(r.mode, McpNetMode::Restricted);
        assert_eq!(r.allow_hosts, vec!["example.com"]);
    }

    #[test]
    fn broad_audited_network_carries_authorization_and_deny_hosts() {
        let net = broad_audited_network(
            vec!["evil.example".into()],
            "dave".into(),
            1_700_000_000_000,
        );
        assert_eq!(net.mode, McpNetMode::BroadAudited);
        assert!(net.allow_hosts.is_empty());
        assert_eq!(net.deny_hosts, vec!["evil.example"]);
        let auth = net.authorization.expect("authorization must be set");
        assert_eq!(auth.authorized_by, "dave");
        assert_eq!(auth.authorized_at_ms, 1_700_000_000_000);
    }

    #[test]
    fn network_policy_from_args_never_carries_authorization() {
        // Non-BroadAudited modes must never carry an authorization record,
        // even if one existed before (mode-change-away-from-BroadAudited
        // clears it — enforced by cmd_mcp_set_network always calling this
        // path when broad_audited=false).
        assert!(
            network_policy_from_args(vec!["example.com".into()], false)
                .unwrap()
                .authorization
                .is_none()
        );
        assert!(
            network_policy_from_args(vec![], true)
                .unwrap()
                .authorization
                .is_none()
        );
    }

    #[test]
    fn record_broad_audited_enabled_appends_an_event() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path();
        let now = chrono::Utc::now();

        record_broad_audited_enabled(mur_home, "carol", "browser", now).unwrap();

        let path = mur_home
            .join("traces")
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains(mur_common::telemetry::METHOD_EGRESS_BROAD_AUDITED_ENABLED));
        assert!(contents.contains("carol"));
        assert!(contents.contains("browser"));
    }
}
