//! `mur agent mcp set-network` — per-server outbound egress policy.
//!
//! Policy construction, the BroadAudited grant and its audit event, and the
//! `host:port` / `--allow-port` pairing (#1619).

use anyhow::{Context, Result, bail};
use mur_common::agent::{EgressAuthorization, McpNetMode, McpServerNetwork, NetworkOutboundMode};
use mur_common::telemetry::{METHOD_BROWSER_PROFILE_BOUND, METHOD_EGRESS_BROAD_AUDITED_ENABLED};

use super::perm::{add_port, is_base_port};
use super::{load_profile_for_edit, resolve_mur_home, save_profile};

/// Split `host[:port]` entries into the portless hosts the egress proxy can
/// actually match, plus the extra (non-web) outbound ports to grant (#1619).
///
/// The proxy strips the port from a CONNECT target before matching, so a
/// stored `host:9000` would never match anything. And the proxy runs INSIDE
/// the agent's seal, so an allowed host on a port outside the web set still
/// dies at the OS port gate (502). Hence: a non-web port must be granted with
/// `--allow-port` in the same command, or the whole command is refused.
/// Web ports (80/443/8080/8443) are already open and are simply stripped.
fn split_host_ports(
    allow_hosts: &[String],
    allow_ports: &[u16],
) -> Result<(Vec<String>, Vec<u16>)> {
    let mut hosts = Vec::with_capacity(allow_hosts.len());
    let mut missing: Vec<(String, u16)> = Vec::new();
    for raw in allow_hosts {
        let g = raw.trim();
        if g.is_empty() {
            bail!("empty --allow-host value");
        }
        if g.contains("://") || g.contains('/') || g.contains(char::is_whitespace) {
            bail!(
                "'{raw}' is not a hostname — pass a bare host (`api.example.com`), a wildcard (`*.example.com`), an IP, or host:port"
            );
        }
        let (host, port) = split_port(g);
        if let Some(p) = port {
            let port: u16 = p
                .parse()
                .ok()
                .filter(|n| *n != 0)
                .ok_or_else(|| anyhow::anyhow!("'{raw}': '{p}' is not a dialable TCP port"))?;
            if !is_base_port(port) && !allow_ports.contains(&port) {
                missing.push((g.to_string(), port));
            }
        }
        let n = mur_common::host_pattern::normalize_host_pattern(host)
            .map_err(|e| anyhow::anyhow!("'{raw}' is not a hostname — {e}"))?;
        // `normalize_host_pattern` brackets IPv6, matching what the proxy sees.
        let host = n.value;
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    if let Some((entry, port)) = missing.first() {
        let mut ports: Vec<u16> = missing.iter().map(|(_, p)| *p).collect();
        ports.sort_unstable();
        ports.dedup();
        let flags = ports
            .iter()
            .map(|p| format!("--allow-port {p}"))
            .collect::<Vec<_>>()
            .join(" ");
        bail!(
            "'{entry}' needs outbound port {port}, which this agent's OS sandbox blocks — allowing the host \
             alone would still fail (502 from the egress proxy).\n\
             Re-run with `{flags}` to grant host and port together.\n\
             WARNING: a port grant opens that port to ANY host for the WHOLE agent — the sandbox port rule \
             does not bind a host. Only add it if nothing else in this agent should reach that port directly."
        );
    }
    let mut grants = Vec::new();
    for &p in allow_ports {
        if p == 0 {
            bail!("port 0 is not dialable");
        }
        if !is_base_port(p) {
            add_port(&mut grants, p);
        }
    }
    Ok((hosts, grants))
}

/// `host:port` → (`host`, Some(port)); `[v6]:port` keeps the brackets (that is
/// what the proxy's `rsplit_once(':')` leaves as the host). A bare IPv6
/// literal (several colons, no brackets) has no port.
fn split_port(g: &str) -> (&str, Option<&str>) {
    if g.starts_with('[') {
        return match g.rsplit_once("]:") {
            Some((h, p)) => (&g[..h.len() + 1], Some(p)),
            None => (g, None),
        };
    }
    match g.split_once(':') {
        Some((h, p)) if !p.contains(':') => (h, Some(p)),
        _ => (g, None),
    }
}

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
pub(crate) fn broad_audited_network(
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
pub(crate) fn record_broad_audited_enabled(
    mur_home: &std::path::Path,
    agent: &str,
    server_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    append_trace(
        mur_home,
        now,
        serde_json::json!({
            "ts": now.to_rfc3339(),
            "method": METHOD_EGRESS_BROAD_AUDITED_ENABLED,
            "mur.agent.name": agent,
            "mur.mcp.server_id": server_id,
        }),
    )
}

/// Append a `mur.browser.profile_bound` event: `agent`'s `server_id` entry now
/// launches with saved profile `new` (was `old`, `null` when none) (#2199).
pub(crate) fn record_profile_bound(
    mur_home: &std::path::Path,
    agent: &str,
    server_id: &str,
    old: Option<&str>,
    new: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    append_trace(
        mur_home,
        now,
        serde_json::json!({
            "ts": now.to_rfc3339(),
            "method": METHOD_BROWSER_PROFILE_BOUND,
            "mur.agent.name": agent,
            "mur.mcp.server_id": server_id,
            "mur.browser.profile.old": old,
            "mur.browser.profile.new": new,
        }),
    )
}

/// Append one JSON line to today's `traces/<YYYY-MM-DD>.jsonl`.
fn append_trace(
    mur_home: &std::path::Path,
    now: chrono::DateTime<chrono::Utc>,
    line: serde_json::Value,
) -> Result<()> {
    let traces_dir = mur_home.join("traces");
    std::fs::create_dir_all(&traces_dir)
        .with_context(|| format!("create {}", traces_dir.display()))?;
    let path = traces_dir
        .join(now.format("%Y-%m-%d").to_string())
        .with_extension("jsonl");

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

/// The stderr preview `set-network` prints before a `Restricted` or
/// `BroadAudited` write; the research caveat is generated, never typed (D1).
fn set_network_preview(
    agent: &str,
    server_id: &str,
    before: Option<&McpServerNetwork>,
    after: &McpServerNetwork,
) -> String {
    crate::cmd::agent::cli::browser_live_preview::render_preview(
        agent,
        server_id,
        before,
        after,
        &[],
        mur_agent_runtime::sandbox::egress_proxy::DENY_OVERLAY_ENFORCEMENT,
    )
}

/// Preview for clearing a server's policy back to `Inherit`. Clearing LIFTS a
/// host bound (the agent's `allow_hosts` does not reach a spawned server), so
/// it is a widening and must be confirmed like any other grant.
fn clear_network_preview(agent: &str, server_id: &str, before: &McpServerNetwork) -> String {
    let was = match before.mode {
        McpNetMode::Restricted => format!("only {}", before.allow_hosts.join(", ")),
        McpNetMode::Off => "no outbound access".to_string(),
        McpNetMode::BroadAudited => "research mode (all hosts, audited)".to_string(),
        McpNetMode::Inherit => "no per-server policy".to_string(),
    };
    format!(
        "`{server_id}` on agent `{agent}` will lose its egress policy (was: {was}).\n\
         It will then be bounded only by the OS sandbox, which restricts PORTS, not hosts — \
         the agent's `allow_hosts` does NOT apply to it.\n\
         Saved to the profile; takes effect after the agent restarts."
    )
}

/// Preview lines for the agent-wide port grants a `set-network` call adds.
/// Only ports not already granted are listed; empty ⇒ nothing to confirm.
fn port_grant_preview(agent: &str, new_ports: &[u16], mode: NetworkOutboundMode) -> Option<String> {
    if new_ports.is_empty() {
        return None;
    }
    let list = new_ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = format!(
        "Also grants outbound TCP port(s) {list} to the WHOLE agent `{agent}`: \
         that opens port(s) {list} to ANY host, not just this server's hosts — \
         the OS sandbox port rule does not bind a host."
    );
    if mode != NetworkOutboundMode::Restricted {
        out.push_str(
            "\n(The agent's outbound mode is not `restricted`, so the port grant changes nothing until it is.)",
        );
    }
    Some(out)
}

/// Everything a non-research `set-network` write must show before it saves:
/// a `Restricted` host diff, a clear-to-`Inherit` (a widening), and any new
/// agent-wide port grants. `None` ⇒ nothing widens (e.g. `--off`, or clearing
/// an already-clear server with no ports), so no confirmation is needed.
fn set_network_confirm_preview(
    agent: &str,
    server_id: &str,
    before: Option<&McpServerNetwork>,
    after: Option<&McpServerNetwork>,
    ports_note: Option<String>,
) -> Option<String> {
    let mut parts = Vec::new();
    match (before, after) {
        (_, Some(net)) if net.mode == McpNetMode::Restricted => {
            parts.push(set_network_preview(agent, server_id, before, net));
        }
        (Some(b), None) if b.mode != McpNetMode::Inherit => {
            parts.push(clear_network_preview(agent, server_id, b));
        }
        _ => {}
    }
    parts.extend(ports_note);
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Set (or clear) a per-server egress policy on `server_id`. Restart the agent
/// to apply (the sandbox + proxy are wired at supervisor startup).
///
/// `broad_audited=true` requests an allow-ALL-except-`deny_hosts` grant,
/// routed through the audited egress proxy — a permission-required action.
/// Unless `yes` is set, prompts for explicit `[y/N]` consent on stdin before
/// writing the authorization record and emitting telemetry.
#[allow(clippy::too_many_arguments)]
pub fn cmd_mcp_set_network(
    agent: &str,
    server_id: &str,
    allow_hosts: Vec<String>,
    allow_ports: Vec<u16>,
    deny_hosts: Vec<String>,
    off: bool,
    broad_audited: bool,
    yes: bool,
) -> Result<()> {
    if !allow_ports.is_empty() && allow_hosts.is_empty() && !broad_audited {
        // Without a host this command would CLEAR the server's policy.
        bail!(
            "--allow-port needs --allow-host (or --broad-audited). For a port grant alone: \
             `mur agent perm allow-port {agent} <port>`"
        );
    }
    let (allow_hosts, port_grants) = split_host_ports(&allow_hosts, &allow_ports)?;
    let (path, mut profile) = load_profile_for_edit(agent)?;
    let outbound_mode = profile.entitlements.network.outbound.mode;
    let new_ports: Vec<u16> = port_grants
        .iter()
        .copied()
        .filter(|p| {
            !profile
                .entitlements
                .network
                .outbound
                .allow_ports
                .contains(p)
        })
        .collect();
    let ports_note = port_grant_preview(agent, &new_ports, outbound_mode);
    let srv = profile
        .mcp_servers
        .iter_mut()
        .find(|s| s.name == server_id)
        .ok_or_else(|| anyhow::anyhow!("MCP server '{server_id}' not found on '{agent}'"))?;

    let before = srv.network.clone();
    if broad_audited {
        let preview_net = broad_audited_network(deny_hosts.clone(), String::new(), 0);
        eprintln!(
            "{}",
            set_network_preview(agent, server_id, before.as_ref(), &preview_net)
        );
        if let Some(note) = &ports_note {
            eprintln!("\n{note}");
        }
        eprintln!("\nApprove broad-audited egress?");
        crate::cmd::consent::confirm_grant_on_terminal(yes)?;

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
        let after = network_policy_from_args(allow_hosts, off);
        if let Some(preview) = set_network_confirm_preview(
            agent,
            server_id,
            before.as_ref(),
            after.as_ref(),
            ports_note,
        ) {
            eprintln!("{preview}");
            crate::cmd::consent::confirm_grant_on_terminal(yes)?;
        }
        srv.network = after;
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
    if !port_grants.is_empty() {
        let outbound = &mut profile.entitlements.network.outbound;
        for &p in &port_grants {
            add_port(&mut outbound.allow_ports, p);
        }
        let mode = outbound.mode;
        save_profile(&path, &mut profile)?;
        let list = port_grants
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "Granted outbound TCP port(s) {list} to agent '{agent}'.\n\
             WARNING: this opens port(s) {list} to ANY host for the WHOLE agent, not just this server's \
             hosts — the OS sandbox port rule does not bind a host. Revoke with \
             `mur agent perm deny-port {agent} <port>`."
        );
        if mode != NetworkOutboundMode::Restricted {
            println!(
                "NOTE: the agent's outbound mode is not `restricted`, so the port grant changes nothing \
                 until it is (`mur agent perm list-ports {agent}`)."
            );
        }
    }
    println!("Restart the agent to apply.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn split_host_ports_strips_web_ports_without_a_grant() {
        // 443 is in the base web set: the port is implied, the host is kept portless.
        let (h, p) = split_host_ports(&hosts(&["api.example.com:443", "*.x.dev"]), &[]).unwrap();
        assert_eq!(h, hosts(&["api.example.com", "*.x.dev"]));
        assert!(p.is_empty());
    }

    #[test]
    fn split_host_ports_refuses_non_web_port_without_allow_port() {
        let err = split_host_ports(&hosts(&["db.internal:9000"]), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("--allow-port 9000"), "{err}");
        assert!(err.contains("ANY host"), "{err}");
    }

    #[test]
    fn split_host_ports_grants_port_when_asked_together() {
        let (h, p) =
            split_host_ports(&hosts(&["db.internal:9000", "[::1]:9000"]), &[9000]).unwrap();
        // Brackets kept: the proxy's rsplit_once(':') leaves `[::1]` as the host.
        assert_eq!(h, hosts(&["db.internal", "[::1]"]));
        assert_eq!(p, vec![9000]);
    }

    #[test]
    fn split_host_ports_drops_base_ports_from_grants_and_rejects_zero() {
        let (_, p) = split_host_ports(&hosts(&["a.com"]), &[443, 9000, 9000]).unwrap();
        assert_eq!(p, vec![9000]);
        assert!(split_host_ports(&hosts(&["a.com"]), &[0]).is_err());
        assert!(split_host_ports(&hosts(&["a.com:0"]), &[]).is_err());
    }

    #[test]
    fn split_host_ports_rejects_urls_and_brackets_bare_ipv6() {
        assert!(split_host_ports(&hosts(&["https://a.com"]), &[]).is_err());
        assert!(split_host_ports(&hosts(&["pro360"]), &[]).is_err());
        assert!(split_host_ports(&hosts(&["評分，"]), &[]).is_err());
        let (h, _) = split_host_ports(&hosts(&["::1"]), &[]).unwrap();
        assert_eq!(h, hosts(&["[::1]"]));
    }

    #[test]
    fn set_network_restricted_preview_lists_diff() {
        let before = network_policy_from_args(hosts(&["old.com", "keep.com"]), false);
        let after = network_policy_from_args(hosts(&["keep.com", "new.com"]), false).unwrap();
        let p = set_network_preview("a1", "browser", before.as_ref(), &after);
        assert!(p.contains("+ new.com") && p.contains("- old.com"), "{p}");
        assert!(
            p.contains("  keep.com") && p.contains("`browser` on agent `a1`"),
            "{p}"
        );
    }

    #[test]
    fn clearing_a_policy_requires_a_preview() {
        let before = network_policy_from_args(hosts(&["a.com"]), false);
        let p = set_network_confirm_preview("a1", "browser", before.as_ref(), None, None)
            .expect("clearing a host bound must be confirmed");
        assert!(
            p.contains("lose its egress policy") && p.contains("only a.com"),
            "{p}"
        );
        assert!(p.contains("does NOT apply"), "{p}");
        // Already clear and no ports: nothing widens, nothing to confirm.
        assert!(set_network_confirm_preview("a1", "browser", None, None, None).is_none());
        // --off narrows: no confirmation.
        let off = network_policy_from_args(vec![], true);
        assert!(
            set_network_confirm_preview("a1", "browser", before.as_ref(), off.as_ref(), None)
                .is_none()
        );
    }

    #[test]
    fn port_grants_appear_in_the_preview_before_saving() {
        let after = network_policy_from_args(hosts(&["db.internal"]), false);
        let note = port_grant_preview("a1", &[9000], NetworkOutboundMode::Restricted);
        let p = set_network_confirm_preview("a1", "browser", None, after.as_ref(), note).unwrap();
        assert!(p.contains("+ db.internal"), "{p}");
        assert!(
            p.contains("port(s) 9000") && p.contains("WHOLE agent") && p.contains("ANY host"),
            "{p}"
        );
        // A port grant alone (e.g. with --off) still needs confirmation.
        let off = network_policy_from_args(vec![], true);
        let note = port_grant_preview("a1", &[9000], NetworkOutboundMode::Restricted);
        assert!(set_network_confirm_preview("a1", "browser", None, off.as_ref(), note).is_some());
        // Already-granted ports are filtered upstream; none ⇒ no note.
        assert!(port_grant_preview("a1", &[], NetworkOutboundMode::Restricted).is_none());
    }

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
        let _envg = mur_common::test_env::EnvGuard::hold();
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

    #[test]
    fn record_profile_bound_appends_old_and_new_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let now = chrono::Utc::now();
        record_profile_bound(tmp.path(), "carol", "browser", None, "deepseek", now).unwrap();
        record_profile_bound(
            tmp.path(),
            "carol",
            "browser",
            Some("deepseek"),
            "gitea",
            now,
        )
        .unwrap();

        let path = tmp
            .path()
            .join("traces")
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["method"], METHOD_BROWSER_PROFILE_BOUND);
        assert_eq!(lines[0]["mur.agent.name"], "carol");
        assert_eq!(lines[0]["mur.mcp.server_id"], "browser");
        assert!(lines[0]["mur.browser.profile.old"].is_null());
        assert_eq!(lines[0]["mur.browser.profile.new"], "deepseek");
        assert_eq!(lines[1]["mur.browser.profile.old"], "deepseek");
    }
}
