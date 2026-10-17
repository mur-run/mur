//! Find and remove stored host entries the write-time validator would refuse
//! (#1906). Entries written before #1905 can hold anything — CJK sentences,
//! dot-less words — and they sit in the profile matching nothing. One pass
//! over every place a host list lives, so `mur doctor`, `--fix` and the
//! profile-load warning cannot disagree about what is invalid.

use super::normalize_host_pattern;
use crate::agent::{AgentProfile, McpNetMode};

/// Which host list an entry lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostList {
    /// `entitlements.network.outbound.allow_hosts`.
    AgentAllow,
    /// `mcp_servers[server].network.allow_hosts`.
    McpAllow(String),
    /// `mcp_servers[server].network.deny_hosts`.
    McpDeny(String),
}

impl std::fmt::Display for HostList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentAllow => write!(f, "agent outbound allow_hosts"),
            Self::McpAllow(s) => write!(f, "MCP server `{s}` allow_hosts"),
            Self::McpDeny(s) => write!(f, "MCP server `{s}` deny_hosts"),
        }
    }
}

/// One stored value the validator refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidHost {
    pub list: HostList,
    pub value: String,
    pub reason: String,
}

/// Every invalid entry on `profile`, in profile order.
pub fn invalid_hosts(profile: &AgentProfile) -> Vec<InvalidHost> {
    let mut out = Vec::new();
    let mut scan = |list: HostList, hosts: &[String]| {
        for h in hosts {
            if let Err(e) = normalize_host_pattern(h) {
                out.push(InvalidHost {
                    list: list.clone(),
                    value: h.clone(),
                    reason: e.to_string(),
                });
            }
        }
    };
    scan(
        HostList::AgentAllow,
        &profile.entitlements.network.outbound.allow_hosts,
    );
    for e in &profile.mcp_servers {
        if let Some(n) = &e.network {
            scan(HostList::McpAllow(e.name.clone()), &n.allow_hosts);
            scan(HostList::McpDeny(e.name.clone()), &n.deny_hosts);
        }
    }
    out
}

/// What [`remove_invalid_hosts`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HostRepair {
    pub removed: Vec<InvalidHost>,
    /// `Restricted` servers left with no hosts: they now reach nothing. Flagged
    /// for the user; the mode is never switched on their behalf.
    pub emptied_restricted: Vec<String>,
}

/// Drop every invalid entry from `profile` in memory. Valid entries, their
/// order, and every mode are left exactly as they were.
pub fn remove_invalid_hosts(profile: &mut AgentProfile) -> HostRepair {
    let removed = invalid_hosts(profile);
    if removed.is_empty() {
        return HostRepair::default();
    }
    let keep = |v: &mut Vec<String>| v.retain(|h| normalize_host_pattern(h).is_ok());
    keep(&mut profile.entitlements.network.outbound.allow_hosts);
    let mut emptied_restricted = Vec::new();
    for e in &mut profile.mcp_servers {
        let Some(n) = e.network.as_mut() else {
            continue;
        };
        let had = !n.allow_hosts.is_empty();
        keep(&mut n.allow_hosts);
        keep(&mut n.deny_hosts);
        if had && n.allow_hosts.is_empty() && n.mode == McpNetMode::Restricted {
            emptied_restricted.push(e.name.clone());
        }
    }
    HostRepair {
        removed,
        emptied_restricted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{McpServerEntry, McpServerNetwork};

    fn profile() -> AgentProfile {
        let yaml = r#"schema: 1
id: 01900000-0000-7000-8000-000000000000
name: mur
display_name: "MUR"
version: "0.1.0"
persona:
  category: custom
  description: "test"
  traits: { tone: neutral, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "llama3.2:3b", params: { temperature: 0.2, max_tokens: 4096 } }
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: { enabled: false, bind: "" }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: []
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: [] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: false }
created_at: "2026-01-01T00:00:00Z"
updated_at: "2026-01-01T00:00:00Z"
"#;
        let mut p: AgentProfile = serde_yaml_ng::from_str(yaml).expect("minimal profile");
        p.entitlements.network.outbound.allow_hosts =
            vec!["api.example.com".into(), "評分，".into()];
        let entry = |name: &str, mode, allow: &[&str], deny: &[&str]| McpServerEntry {
            name: name.into(),
            network: Some(McpServerNetwork {
                mode,
                allow_hosts: allow.iter().map(|s| (*s).into()).collect(),
                deny_hosts: deny.iter().map(|s| (*s).into()).collect(),
                ..Default::default()
            }),
            ..serde_yaml_ng::from_str("name: x\ncommand: x").unwrap()
        };
        p.mcp_servers = vec![
            entry(
                "browser",
                McpNetMode::Restricted,
                &["查台灣居家修繕市場規模", "／", "競品"],
                &[],
            ),
            entry(
                "search",
                McpNetMode::BroadAudited,
                &[],
                &["evil.example", "a"],
            ),
            entry(
                "shop",
                McpNetMode::Restricted,
                &["shop.example.com", "x y"],
                &[],
            ),
        ];
        p
    }

    #[test]
    fn reports_every_invalid_entry_with_its_list() {
        let bad = invalid_hosts(&profile());
        let got: Vec<(String, &str)> = bad
            .iter()
            .map(|b| (b.list.to_string(), b.value.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("agent outbound allow_hosts".into(), "評分，"),
                (
                    "MCP server `browser` allow_hosts".into(),
                    "查台灣居家修繕市場規模"
                ),
                ("MCP server `browser` allow_hosts".into(), "／"),
                ("MCP server `browser` allow_hosts".into(), "競品"),
                ("MCP server `search` deny_hosts".into(), "a"),
                ("MCP server `shop` allow_hosts".into(), "x y"),
            ]
        );
        assert!(bad.iter().all(|b| !b.reason.is_empty()));
    }

    #[test]
    fn fix_keeps_valid_hosts_and_flags_emptied_restricted() {
        let mut p = profile();
        let r = remove_invalid_hosts(&mut p);
        assert_eq!(r.removed.len(), 6);
        assert_eq!(r.emptied_restricted, vec!["browser".to_string()]);
        assert_eq!(
            p.entitlements.network.outbound.allow_hosts,
            vec!["api.example.com"]
        );
        let net = |n: &str| {
            p.mcp_servers
                .iter()
                .find(|e| e.name == n)
                .unwrap()
                .network
                .clone()
                .unwrap()
        };
        assert!(net("browser").allow_hosts.is_empty());
        // Never re-moded.
        assert_eq!(net("browser").mode, McpNetMode::Restricted);
        assert_eq!(net("search").deny_hosts, vec!["evil.example"]);
        assert_eq!(net("shop").allow_hosts, vec!["shop.example.com"]);
        assert!(invalid_hosts(&p).is_empty());
    }

    #[test]
    fn clean_profile_is_untouched() {
        let mut p = profile();
        remove_invalid_hosts(&mut p);
        let before = p.clone();
        assert_eq!(remove_invalid_hosts(&mut p), HostRepair::default());
        assert_eq!(p, before);
    }
}
