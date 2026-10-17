//! The effective network policy of one agent, both layers in one table
//! (#1906, spec Part 4). Shared by `mur agent network <agent>` and murmur's
//! `/browser status`, so the two can never describe the same profile
//! differently.
//!
//! The header states the one thing nobody can see from the profile: the
//! agent's own `allow_hosts` does NOT govern its MCP servers. An `Inherit`
//! server is bound only by the OS sandbox, which filters by port.

use mur_common::AgentProfile;
use mur_common::agent::{McpNetMode, McpServerEntry, NetworkOutboundMode};

/// How many hosts a cell lists before it says "+N more".
const MAX_HOSTS_IN_CELL: usize = 4;

/// Mode label for one MCP server. `Inherit` is named for what it does, not
/// for its serialized name (the stored value is unchanged).
fn mode_label(mode: McpNetMode) -> &'static str {
    match mode {
        McpNetMode::Inherit => "any host (port-limited only)",
        McpNetMode::Restricted => "listed hosts only",
        McpNetMode::BroadAudited => "research (all hosts, logged)",
        McpNetMode::Off => "no network",
    }
}

fn host_cell(hosts: &[String]) -> String {
    if hosts.is_empty() {
        return "—".to_owned();
    }
    let shown: Vec<&str> = hosts
        .iter()
        .take(MAX_HOSTS_IN_CELL)
        .map(String::as_str)
        .collect();
    let more = hosts.len().saturating_sub(MAX_HOSTS_IN_CELL);
    if more > 0 {
        format!("{}, +{more} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// `(server, mode, hosts, warning)` for one entry.
fn row(e: &McpServerEntry) -> (String, &'static str, String, Option<&'static str>) {
    let net = e.network.clone().unwrap_or_default();
    let (hosts, warn) = match net.mode {
        McpNetMode::Inherit => ("—".to_owned(), Some("⚠ not host-limited")),
        McpNetMode::Restricted if net.allow_hosts.is_empty() => {
            ("—".to_owned(), Some("⚠ reaches nothing"))
        }
        McpNetMode::Restricted => (host_cell(&net.allow_hosts), None),
        McpNetMode::BroadAudited => (format!("deny: {} entr(ies)", net.deny_hosts.len()), None),
        McpNetMode::Off => ("—".to_owned(), None),
    };
    (e.name.clone(), mode_label(net.mode), hosts, warn)
}

fn agent_line(p: &AgentProfile) -> String {
    let o = &p.entitlements.network.outbound;
    let mode = match o.mode {
        NetworkOutboundMode::Restricted => format!("restricted ({})", host_cell(&o.allow_hosts)),
        other => format!("{other:?}").to_lowercase(),
    };
    format!(
        "agent `{}` — outbound: {mode}  ← governs only MUR's own connections, not its MCP servers",
        p.name
    )
}

/// The whole view as plain text.
pub fn render(p: &AgentProfile) -> String {
    let mut out = vec![agent_line(p)];
    // Only what the runtime would spawn: a disabled server reaches nothing.
    let servers = p.enabled_mcp_servers();
    if servers.is_empty() {
        out.push("no enabled MCP servers".to_owned());
        return out.join("\n");
    }
    let rows: Vec<_> = servers.iter().map(row).collect();
    let w0 = rows
        .iter()
        .map(|r| r.0.chars().count())
        .max()
        .unwrap_or(0)
        .max(6);
    let w1 = rows
        .iter()
        .map(|r| r.1.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    let w2 = rows
        .iter()
        .map(|r| r.2.chars().count())
        .max()
        .unwrap_or(0)
        .max(5);
    out.push(format!(
        "{:<w0$}  {:<w1$}  {:<w2$}",
        "server", "mode", "hosts"
    ));
    for (name, mode, hosts, warn) in &rows {
        let line = format!(
            "{name:<w0$}  {mode:<w1$}  {hosts:<w2$}  {}",
            warn.unwrap_or("")
        );
        out.push(line.trim_end().to_owned());
    }
    if rows.iter().any(|r| r.1 == mode_label(McpNetMode::Inherit)) {
        out.push(
            "⚠ an `any host` server can reach every host on an open port; scope it with \
             `mur agent mcp set-network`."
                .to_owned(),
        );
    }
    out.join("\n")
}

/// `mur agent network <agent>`.
pub fn run(name: &str) -> anyhow::Result<()> {
    let (_, profile) = super::load_profile_for_edit(name)?;
    println!("{}", render(&profile));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::McpServerNetwork;

    fn profile() -> AgentProfile {
        let mut p: AgentProfile = serde_yaml_ng::from_str(YAML).unwrap();
        p.entitlements.network.outbound.allow_hosts = vec!["api.anthropic.com".into()];
        let entry = |name: &str, net: Option<McpServerNetwork>| {
            let mut e: McpServerEntry =
                serde_yaml_ng::from_str(&format!("name: {name}\ncommand: x")).unwrap();
            e.network = net;
            e
        };
        let net = |mode, allow: &[&str], deny: &[&str]| {
            Some(McpServerNetwork {
                mode,
                allow_hosts: allow.iter().map(|s| (*s).into()).collect(),
                deny_hosts: deny.iter().map(|s| (*s).into()).collect(),
                ..Default::default()
            })
        };
        p.mcp_servers = vec![
            entry(
                "browser",
                net(
                    McpNetMode::Restricted,
                    &["shop.example.com", "cdn.shop.example.com"],
                    &[],
                ),
            ),
            entry(
                "search",
                net(McpNetMode::BroadAudited, &[], &["a.x", "b.x", "c.x"]),
            ),
            entry("fetch", None),
            entry("empty", net(McpNetMode::Restricted, &[], &[])),
        ];
        p
    }

    const YAML: &str = include_str!("network_view_fixture.yaml");

    #[test]
    fn one_table_for_both_layers() {
        let v = render(&profile());
        let lines: Vec<&str> = v.lines().collect();
        assert!(lines[0].contains("restricted (api.anthropic.com)"), "{v}");
        assert!(lines[0].contains("not its MCP servers"), "{v}");
        let line = |s: &str| *lines.iter().find(|l| l.starts_with(s)).unwrap();
        assert!(line("browser").contains("listed hosts only"), "{v}");
        assert!(
            line("browser").contains("shop.example.com, cdn.shop.example.com"),
            "{v}"
        );
        assert!(
            line("search").contains("research") && line("search").contains("deny: 3"),
            "{v}"
        );
        assert!(
            line("fetch").contains("any host (port-limited only)"),
            "{v}"
        );
        assert!(line("fetch").contains("⚠ not host-limited"), "{v}");
        assert!(line("empty").contains("⚠ reaches nothing"), "{v}");
        assert!(v.contains("mur agent mcp set-network"), "{v}");
    }

    #[test]
    fn long_host_lists_are_capped() {
        let hosts: Vec<String> = (0..7).map(|i| format!("h{i}.x.com")).collect();
        assert!(host_cell(&hosts).ends_with("+3 more"));
    }
}
