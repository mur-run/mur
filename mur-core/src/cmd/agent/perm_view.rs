//! The one derivation of "what may this agent reach, and is that enforced".
//!
//! `mur agent perm list-hosts` / `list-paths` render text from this; the Hub
//! serialises it into `AgentDetail`. One derivation, two surfaces, so the two
//! facts that matter cannot drift between them: runtime traffic is not
//! MCP-server traffic, and a configured grant is not an enforced one.

use mur_common::LockFile;
use mur_common::agent::{
    McpNetMode, NetworkOutboundMode, SpawnMode, ToolPolicy, filesystem_grants_digest,
};
use mur_common::bridge::llm_entitlement::LlmMode;
use mur_common::hitl::RiskTier;
use serde::{Deserialize, Serialize};

/// What the running agent's sandbox seal says — derived from `running.lock`,
/// never from the profile, because the profile is a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// No lock: nothing is enforced until the agent starts.
    NotRunning,
    /// A lock with no seal record: an older runtime; what took effect is unknown.
    SealUnknown,
    /// Sealed with `enforcing: false`: only advisory hooks; the agent can reach
    /// MORE than the grants list.
    Advisory,
    Enforcing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GrantStatus {
    /// No seal to check against (not running, or seal unknown).
    Unverified,
    Effective,
    /// The sandbox discarded this grant when sealing.
    Dropped {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathGrantView {
    pub raw: String,
    pub expanded: String,
    pub status: GrantStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PathsView {
    pub read: Vec<PathGrantView>,
    pub write: Vec<PathGrantView>,
    pub deny: Vec<PathGrantView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundView {
    pub mode: NetworkOutboundMode,
    pub allow_hosts: Vec<String>,
    /// In `restricted` / `proxy_only` the configured model's own host is
    /// reachable whether or not it is listed.
    pub model_host_always_allowed: bool,
}

/// What bounds an MCP server's traffic. NEVER the agent's `allow_hosts` —
/// that list guards the runtime's own HTTP client, and a spawned server does
/// not run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    /// `inherit`: only the OS sandbox, which restricts ports, not hosts.
    Unbounded,
    /// `restricted`: the server's own `allow_hosts`, via the egress proxy.
    OwnHosts,
    /// `broad_audited`: all hosts (minus `deny_hosts`), audited.
    AllAudited,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpNetView {
    pub name: String,
    pub mode: McpNetMode,
    pub scope: McpScope,
    pub allow_hosts: Vec<String>,
    pub deny_hosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessesView {
    pub spawn_mode: SpawnMode,
    pub allowed: Vec<String>,
    pub allowed_dirs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRuleView {
    pub pattern: String,
    pub policy: ToolPolicy,
    pub risk: Option<RiskTier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsView {
    pub cpu_seconds: Option<u64>,
    pub memory_mb: u64,
    pub file_descriptors: u32,
    pub processes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionsView {
    pub enforcement: Enforcement,
    /// `SandboxRecord.mode` when a seal exists (`"macos-sbpl"`, `"advisory-only"`, …).
    pub sandbox_mode: Option<String>,
    /// Filesystem grants were edited after the seal: `Effective` rows describe
    /// the profile as it is now, not as it was enforced.
    pub grants_drifted: bool,
    pub runtime_outbound: OutboundView,
    pub mcp_servers: Vec<McpNetView>,
    pub filesystem: PathsView,
    pub processes: ProcessesView,
    pub tools: Vec<ToolRuleView>,
    pub llm: LlmMode,
    pub limits: LimitsView,
    pub fail_closed_on_sandbox_error: bool,
}

pub fn permissions_view(
    profile: &mur_common::AgentProfile,
    lock: Option<&LockFile>,
) -> PermissionsView {
    let ent = &profile.entitlements;
    let sandbox = lock.and_then(|l| l.sandbox.as_ref());
    let enforcement = match (lock, sandbox) {
        (None, _) => Enforcement::NotRunning,
        (Some(_), None) => Enforcement::SealUnknown,
        (Some(_), Some(sb)) if !sb.enforcing => Enforcement::Advisory,
        (Some(_), Some(_)) => Enforcement::Enforcing,
    };
    let grants_drifted =
        sandbox.is_some_and(|sb| sb.granted_digest != filesystem_grants_digest(&ent.filesystem));

    let grants = |verb: &str, list: &[String]| -> Vec<PathGrantView> {
        list.iter()
            .map(|raw| {
                let expanded = mur_agent_runtime::sandbox::policy::expand_entitlement_path(raw)
                    .display()
                    .to_string();
                let status = match sandbox {
                    None => GrantStatus::Unverified,
                    Some(sb) => {
                        match sb
                            .dropped
                            .iter()
                            .find(|d| d.verb == verb && d.path == expanded)
                        {
                            Some(d) => GrantStatus::Dropped {
                                reason: d.reason.clone(),
                            },
                            None => GrantStatus::Effective,
                        }
                    }
                };
                PathGrantView {
                    raw: raw.clone(),
                    expanded,
                    status,
                }
            })
            .collect()
    };

    let out = &ent.network.outbound;
    let model_host_always_allowed = !matches!(
        out.mode,
        NetworkOutboundMode::Off | NetworkOutboundMode::Unrestricted
    );

    let mcp_servers = profile
        .mcp_servers
        .iter()
        .map(|m| {
            let mode = m.network.as_ref().map(|n| n.mode).unwrap_or_default();
            let scope = match mode {
                McpNetMode::Inherit => McpScope::Unbounded,
                McpNetMode::Restricted => McpScope::OwnHosts,
                McpNetMode::BroadAudited => McpScope::AllAudited,
                McpNetMode::Off => McpScope::Off,
            };
            McpNetView {
                name: m.name.clone(),
                mode,
                scope,
                allow_hosts: m
                    .network
                    .as_ref()
                    .map(|n| n.allow_hosts.clone())
                    .unwrap_or_default(),
                deny_hosts: m
                    .network
                    .as_ref()
                    .map(|n| n.deny_hosts.clone())
                    .unwrap_or_default(),
            }
        })
        .collect();

    PermissionsView {
        enforcement,
        sandbox_mode: sandbox.map(|sb| sb.mode.clone()),
        grants_drifted,
        runtime_outbound: OutboundView {
            mode: out.mode,
            allow_hosts: out.allow_hosts.clone(),
            model_host_always_allowed,
        },
        mcp_servers,
        filesystem: PathsView {
            read: grants("read", &ent.filesystem.read),
            write: grants("write", &ent.filesystem.write),
            deny: grants("deny", &ent.filesystem.deny),
        },
        processes: ProcessesView {
            spawn_mode: ent.processes.spawn.mode,
            allowed: ent.processes.spawn.allowed.clone(),
            allowed_dirs: ent.processes.spawn.allowed_dirs.clone(),
        },
        tools: ent
            .tools
            .iter()
            .map(|r| ToolRuleView {
                pattern: r.pattern.clone(),
                policy: r.policy,
                risk: r.risk,
            })
            .collect(),
        llm: ent.llm.mode,
        limits: LimitsView {
            cpu_seconds: ent.limits.cpu_seconds,
            memory_mb: ent.limits.memory_mb,
            file_descriptors: ent.limits.file_descriptors,
            processes: ent.limits.processes,
        },
        fail_closed_on_sandbox_error: ent.fail_closed_on_sandbox_error,
    }
}

// ── CLI text renderers ────────────────────────────────────────────────
//
// They live beside the derivation rather than beside the command handlers,
// so a change to the view and to the text describing it land in one file.

/// Testable core of [`print_outbound_picture`].
pub(super) fn outbound_picture(profile: &mur_common::AgentProfile) -> String {
    use mur_common::agent::NetworkOutboundMode;
    use std::fmt::Write as _;

    let v = permissions_view(profile, None);
    let out = &v.runtime_outbound;
    let mut o = String::new();

    let _ = writeln!(o, "runtime's own traffic — {:?}", out.mode);
    let _ = writeln!(
        o,
        "  (in-process DNS guard + the B0 gate on `network.*` tools)"
    );
    match out.mode {
        NetworkOutboundMode::Off => {
            let _ = writeln!(o, "  no outbound");
        }
        NetworkOutboundMode::Unrestricted => {
            let _ = writeln!(o, "  any host, any port");
        }
        _ => {
            if out.allow_hosts.is_empty() {
                let _ = writeln!(
                    o,
                    "  allow_hosts: (none — only the configured model's host)"
                );
            } else {
                let _ = writeln!(o, "  allow_hosts:");
                for h in &out.allow_hosts {
                    let _ = writeln!(o, "    {h}");
                }
                let _ = writeln!(o, "  plus the configured model's own host, always allowed");
            }
        }
    }

    if v.mcp_servers.is_empty() {
        return o;
    }
    let _ = writeln!(o);
    let _ = writeln!(o, "MCP servers — {}", v.mcp_servers.len());
    let mut any_inherit = false;
    for m in &v.mcp_servers {
        let detail = match m.scope {
            // The load-bearing line: `inherit` does NOT pick up allow_hosts.
            McpScope::Unbounded => {
                any_inherit = true;
                "NOT bounded by allow_hosts above — only by the OS sandbox, which \
                 restricts ports, not hosts"
                    .to_string()
            }
            McpScope::OwnHosts => {
                if m.allow_hosts.is_empty() {
                    "via the egress proxy; no hosts allowed (denies all)".to_string()
                } else {
                    format!("via the egress proxy; allows {}", m.allow_hosts.join(", "))
                }
            }
            McpScope::AllAudited => {
                if m.deny_hosts.is_empty() {
                    "via the egress proxy; ALL hosts, audited".to_string()
                } else {
                    format!(
                        "via the egress proxy; all hosts except {}, audited",
                        m.deny_hosts.join(", ")
                    )
                }
            }
            McpScope::Off => "no outbound".to_string(),
        };
        let label = format!("{:?}", m.mode).to_lowercase();
        let _ = writeln!(o, "  {:<20} {:<11} {detail}", m.name, label);
    }
    if any_inherit {
        let _ = writeln!(o);
        let _ = writeln!(
            o,
            "  Bound a server by host: mur agent mcp set-network <agent> <server> --allow-host <host>"
        );
    }
    o
}

/// Testable core of [`cmd_perm_list_paths`].
pub(super) fn paths_picture(
    name: &str,
    profile: &mur_common::AgentProfile,
    lock: Option<&LockFile>,
) -> String {
    use std::fmt::Write as _;

    let v = permissions_view(profile, lock);
    let mut o = String::new();
    let mode = v.sandbox_mode.as_deref().unwrap_or_default();

    // The header can subsume everything below it, so it goes first.
    match v.enforcement {
        Enforcement::NotRunning => {
            let _ = writeln!(
                o,
                "agent '{name}' is not running — these are the grants it would ask for; \
                 nothing is enforced until it starts."
            );
        }
        Enforcement::SealUnknown => {
            let _ = writeln!(
                o,
                "agent '{name}' was started by a runtime that did not record its seal, \
                 so what actually took effect is unknown. Restart it to find out."
            );
        }
        Enforcement::Advisory => {
            let _ = writeln!(
                o,
                "agent '{name}' is running WITHOUT a kernel sandbox ({mode}). Only advisory \
                 hooks apply, so it can reach MORE than the grants below — restart it to \
                 try sealing again."
            );
        }
        Enforcement::Enforcing => {
            let _ = writeln!(o, "agent '{name}' — sandbox enforcing ({mode})");
        }
    }

    let fs = &v.filesystem;
    for (label, list) in [("read", &fs.read), ("write", &fs.write), ("deny", &fs.deny)] {
        if list.is_empty() {
            continue;
        }
        let _ = writeln!(o, "\n{}", label.to_uppercase());
        for g in list {
            match &g.status {
                GrantStatus::Dropped { reason } => {
                    let _ = writeln!(o, "  ✗ {}\n      dropped — {reason}", g.raw);
                }
                GrantStatus::Unverified => {
                    let _ = writeln!(o, "  · {}", g.raw);
                }
                GrantStatus::Effective => {
                    let _ = writeln!(o, "  ✓ {}", g.raw);
                }
            }
        }
    }

    if v.grants_drifted {
        let _ = writeln!(
            o,
            "\nGrants have changed since this agent sealed, so the ✓ rows describe the \
             profile as it is now, not as it was enforced:\n    mur agent restart {name}"
        );
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::{DroppedGrant, McpServerEntry, McpServerNetwork, SandboxRecord};

    fn fs_profile(read: &[&str], write: &[&str]) -> mur_common::AgentProfile {
        let mut p = mur_common::AgentProfile::default_for_tests();
        p.entitlements.filesystem.read = read.iter().map(|s| s.to_string()).collect();
        p.entitlements.filesystem.write = write.iter().map(|s| s.to_string()).collect();
        p
    }

    fn lock_with(sandbox: Option<SandboxRecord>) -> LockFile {
        LockFile {
            schema: 1,
            uuid: "u".into(),
            name: "mur".into(),
            pid: 1,
            ppid: 0,
            started_at: "t".into(),
            binary_version: "v".into(),
            transports: mur_common::agent::LockTransports {
                stdio: true,
                unix_socket: None,
                tcp: None,
                webhook: None,
            },
            card_digest: "d".into(),
            capabilities: vec![],
            build_sha: String::new(),
            proto_version: 0,
            sandbox,
        }
    }

    fn sealed(profile: &mur_common::AgentProfile, dropped: Vec<DroppedGrant>) -> SandboxRecord {
        SandboxRecord {
            enforcing: true,
            mode: "macos-sbpl".into(),
            granted_digest: mur_common::agent::filesystem_grants_digest(
                &profile.entitlements.filesystem,
            ),
            dropped,
        }
    }

    /// Without a running agent nothing is enforced, and the listing must not
    /// imply otherwise by ticking every row.
    #[test]
    fn a_stopped_agent_is_listed_without_claiming_anything_took_effect() {
        let p = fs_profile(&[], &["/tmp/x"]);
        let out = paths_picture("mur", &p, None);
        assert!(out.contains("is not running"), "{out}");
        assert!(out.contains("· /tmp/x"), "{out}");
        assert!(
            !out.contains('✓'),
            "a stopped agent has nothing effective: {out}"
        );
    }

    #[test]
    fn a_dropped_grant_is_marked_with_its_reason() {
        let p = fs_profile(&[], &["/tmp/gone", "/tmp/live"]);
        let rec = sealed(
            &p,
            vec![DroppedGrant {
                path: "/tmp/gone".into(),
                verb: "write".into(),
                reason: "path does not exist on disk".into(),
            }],
        );
        let out = paths_picture("mur", &p, Some(&lock_with(Some(rec))));
        assert!(out.contains("✗ /tmp/gone"), "{out}");
        assert!(out.contains("does not exist on disk"), "{out}");
        assert!(out.contains("✓ /tmp/live"), "{out}");
        assert!(!out.contains("restart"), "nothing drifted: {out}");
    }

    /// The loudest case: no kernel sandbox at all means MORE access than the
    /// grants below, so it cannot be a footnote.
    #[test]
    fn an_unenforced_sandbox_is_stated_before_anything_else() {
        let p = fs_profile(&[], &["/tmp/x"]);
        let mut rec = sealed(&p, vec![]);
        rec.enforcing = false;
        rec.mode = "advisory-only".into();
        let out = paths_picture("mur", &p, Some(&lock_with(Some(rec))));
        let first = out.lines().next().unwrap_or_default();
        assert!(first.contains("WITHOUT a kernel sandbox"), "{out}");
        assert!(first.contains("MORE"), "{out}");
    }

    /// Editing grants after the seal does not change what is enforced, and the
    /// listing has to say so or its ticks are lies.
    #[test]
    fn grants_edited_after_the_seal_are_flagged_for_restart() {
        let sealed_profile = fs_profile(&[], &["/tmp/x"]);
        let rec = sealed(&sealed_profile, vec![]);
        let now = fs_profile(&[], &["/tmp/x", "/tmp/added-later"]);
        let out = paths_picture("mur", &now, Some(&lock_with(Some(rec))));
        assert!(out.contains("changed since this agent sealed"), "{out}");
        assert!(out.contains("mur agent restart mur"), "{out}");
    }

    /// An agent started by an older runtime has no record; saying nothing would
    /// let its rows read as verified.
    #[test]
    fn a_lock_without_a_seal_record_says_the_effect_is_unknown() {
        let p = fs_profile(&[], &["/tmp/x"]);
        let out = paths_picture("mur", &p, Some(&lock_with(None)));
        assert!(out.contains("did not record its seal"), "{out}");
        assert!(!out.contains('✓'), "{out}");
    }

    fn profile_with(
        servers: Vec<(&str, Option<mur_common::agent::McpNetMode>)>,
    ) -> mur_common::AgentProfile {
        use mur_common::agent::{McpServerEntry, McpServerNetwork};
        let mut p = mur_common::AgentProfile::default_for_tests();
        p.entitlements.network.outbound.allow_hosts = vec!["api.example.com".into()];
        for (name, mode) in servers {
            p.mcp_servers.push(McpServerEntry {
                name: name.into(),
                command: "npx".into(),
                network: mode.map(|m| McpServerNetwork {
                    mode: m,
                    allow_hosts: vec!["db.internal".into()],
                    deny_hosts: vec![],
                    authorization: None,
                }),
                ..Default::default()
            });
        }
        p
    }

    /// The whole point of the view: an `inherit` server must be shown as NOT
    /// covered by the agent's allow_hosts. Believing otherwise is what sent
    /// someone chasing a MySQL connection that the host list never governed.
    #[test]
    fn inherit_servers_are_shown_as_not_bounded_by_allow_hosts() {
        let p = profile_with(vec![("media", None)]);
        let out = outbound_picture(&p);
        assert!(out.contains("api.example.com"), "{out}");
        assert!(
            out.contains("NOT bounded by allow_hosts"),
            "an inherit server must be called out: {out}"
        );
        assert!(
            out.contains("ports, not hosts"),
            "must name what DOES bound it: {out}"
        );
        assert!(out.contains("set-network"), "must name the remedy: {out}");
    }

    /// A restricted server is genuinely host-bounded — it must NOT carry the
    /// warning, or the warning becomes noise and stops being read.
    #[test]
    fn restricted_servers_show_their_own_hosts_without_the_warning() {
        let p = profile_with(vec![(
            "db",
            Some(mur_common::agent::McpNetMode::Restricted),
        )]);
        let out = outbound_picture(&p);
        assert!(out.contains("db.internal"), "{out}");
        assert!(!out.contains("NOT bounded by allow_hosts"), "{out}");
        assert!(!out.contains("set-network"), "no remedy needed: {out}");
    }

    /// With no MCP servers there is only one policy, and nothing to disambiguate.
    #[test]
    fn no_servers_means_no_server_section() {
        let p = profile_with(vec![]);
        let out = outbound_picture(&p);
        assert!(out.contains("runtime's own traffic"), "{out}");
        assert!(!out.contains("MCP servers"), "{out}");
    }

    fn profile() -> mur_common::AgentProfile {
        let mut p = mur_common::AgentProfile::default_for_tests();
        p.entitlements.filesystem.write = vec!["/tmp/x".into()];
        p.entitlements.network.outbound.allow_hosts = vec!["api.example.com".into()];
        p
    }

    fn sealed_rec(p: &mur_common::AgentProfile, enforcing: bool) -> SandboxRecord {
        SandboxRecord {
            enforcing,
            mode: if enforcing {
                "macos-sbpl"
            } else {
                "advisory-only"
            }
            .into(),
            granted_digest: filesystem_grants_digest(&p.entitlements.filesystem),
            dropped: vec![],
        }
    }

    #[test]
    fn enforcement_covers_all_four_states() {
        let p = profile();
        assert_eq!(
            permissions_view(&p, None).enforcement,
            Enforcement::NotRunning
        );
        assert_eq!(
            permissions_view(&p, Some(&lock_with(None))).enforcement,
            Enforcement::SealUnknown
        );
        assert_eq!(
            permissions_view(&p, Some(&lock_with(Some(sealed_rec(&p, false))))).enforcement,
            Enforcement::Advisory
        );
        assert_eq!(
            permissions_view(&p, Some(&lock_with(Some(sealed_rec(&p, true))))).enforcement,
            Enforcement::Enforcing
        );
    }

    /// Not running means no grant is verified — the view must not tick rows.
    #[test]
    fn a_stopped_agent_has_only_unverified_grants() {
        let v = permissions_view(&profile(), None);
        assert_eq!(v.filesystem.write.len(), 1);
        assert_eq!(v.filesystem.write[0].raw, "/tmp/x");
        assert_eq!(v.filesystem.write[0].status, GrantStatus::Unverified);
        assert!(!v.grants_drifted);
    }

    #[test]
    fn a_dropped_grant_carries_its_reason_and_drift_is_detected() {
        let mut p = profile();
        p.entitlements.filesystem.write.push("/tmp/gone".into());
        let mut rec = sealed_rec(&p, true);
        rec.dropped.push(DroppedGrant {
            path: "/tmp/gone".into(),
            verb: "write".into(),
            reason: "path does not exist on disk".into(),
        });
        let l = lock_with(Some(rec));
        let v = permissions_view(&p, Some(&l));
        assert_eq!(v.filesystem.write[0].status, GrantStatus::Effective);
        assert_eq!(
            v.filesystem.write[1].status,
            GrantStatus::Dropped {
                reason: "path does not exist on disk".into()
            }
        );
        assert!(!v.grants_drifted);

        p.entitlements
            .filesystem
            .write
            .push("/tmp/added-later".into());
        assert!(permissions_view(&p, Some(&l)).grants_drifted);
    }

    /// The load-bearing case from the spec: an `inherit` server is Unbounded
    /// even though the agent's own allow_hosts is non-empty.
    #[test]
    fn inherit_server_is_unbounded_while_agent_allow_hosts_is_set() {
        let mut p = profile();
        p.mcp_servers.push(McpServerEntry {
            name: "media".into(),
            command: "npx".into(),
            network: None,
            ..Default::default()
        });
        p.mcp_servers.push(McpServerEntry {
            name: "db".into(),
            command: "npx".into(),
            network: Some(McpServerNetwork {
                mode: McpNetMode::Restricted,
                allow_hosts: vec!["db.internal".into()],
                deny_hosts: vec![],
                authorization: None,
            }),
            ..Default::default()
        });
        let v = permissions_view(&p, None);
        assert_eq!(v.runtime_outbound.allow_hosts, vec!["api.example.com"]);
        assert!(v.runtime_outbound.model_host_always_allowed);
        assert_eq!(v.mcp_servers[0].scope, McpScope::Unbounded);
        assert_eq!(v.mcp_servers[1].scope, McpScope::OwnHosts);
        assert_eq!(v.mcp_servers[1].allow_hosts, vec!["db.internal"]);
    }

    /// The Hub deserialises what mur-core serialises; a private field or a
    /// non-string enum tag would break at runtime, not compile time.
    #[test]
    fn round_trips_through_json() {
        let p = profile();
        let v = permissions_view(&p, Some(&lock_with(Some(sealed_rec(&p, true)))));
        let s = serde_json::to_string(&v).unwrap();
        let back: PermissionsView = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
        assert!(s.contains("\"enforcement\":\"enforcing\""), "{s}");
        assert!(s.contains("\"status\":\"effective\""), "{s}");
    }
}
