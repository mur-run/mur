//! The one derivation of "what may this agent reach, and is that enforced".
//!
//! `mur agent perm list-hosts` / `list-paths` render text from this; the Hub
//! serialises it into `AgentDetail`. One derivation, two surfaces, so the two
//! facts that matter cannot drift between them: runtime traffic is not
//! MCP-server traffic, and a configured grant is not an enforced one.

use mur_common::LockFile;
use mur_common::agent::{
    McpNetMode, NetworkOutboundMode, SpawnMode, ToolPolicy, filesystem_grants_digest,
    spawn_dirs_digest,
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
    /// The built-in web ports every `restricted` agent may dial (80/443/
    /// 8080/8443). Sent so the Hub shows the real picture rather than only
    /// the user's additions, which on their own look like the whole list
    /// (issue #006).
    pub base_ports: Vec<u16>,
    /// Extra outbound TCP ports the user granted. Honored under `restricted`
    /// only; the Hub marks them inert under the other modes.
    pub allow_ports: Vec<u16>,
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
    /// `allowed_dirs` checked against the seal (#2030): a build-lane grant for
    /// a directory that did not exist at seal time is inactive until restart.
    #[serde(default)]
    pub dir_grants: Vec<PathGrantView>,
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
pub struct OverrideView {
    pub nonce: String,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionsView {
    pub enforcement: Enforcement,
    /// `SandboxRecord.mode` when a seal exists (`"macos-sbpl"`, `"advisory-only"`, …).
    pub sandbox_mode: Option<String>,
    /// Filesystem grants were edited after the seal: `Effective` rows describe
    /// the profile as it is now, not as it was enforced.
    pub grants_drifted: bool,
    /// This boot runs relaxed by a startup override: the nonce and the flags
    /// as the CLI spells them. Expected drift, kept apart from
    /// `grants_drifted` (spec §Visibility). `None` = no override applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relaxed_by_override: Option<OverrideView>,
    pub runtime_outbound: OutboundView,
    pub mcp_servers: Vec<McpNetView>,
    pub filesystem: PathsView,
    pub processes: ProcessesView,
    pub tools: Vec<ToolRuleView>,
    pub llm: LlmMode,
    pub limits: LimitsView,
    pub fail_closed_on_sandbox_error: bool,
    /// Layer-3 intent check (`hitl.intent_check`): the model that may raise
    /// a `bash` call's tier. `None` = off.
    pub intent_check: Option<mur_common::hitl::intent::IntentCheckConfig>,
}

/// Build-lane rows (`processes.spawn.allowed_dirs`) checked against the seal.
///
/// Not [`permissions_view`]'s `grants` closure: that one reads "absent from the
/// dropped list" as "effective", which is only true if the grant was in the
/// profile when the sandbox sealed. Build-lane grants have their own digest, so
/// a row is `Effective` only when the seal both recorded that digest and still
/// agrees with the profile; otherwise it is `Unverified`. A dropped record
/// always wins — it is a fact about the seal, whatever the profile says now.
///
/// The runtime also drops linked-worktree candidates derived from a configured
/// directory (`worktrees_of`), which are not in the profile as strings. They
/// are listed after the configured rows so the reason is not lost.
fn dir_grants(
    dirs: &[String],
    sandbox: Option<&mur_common::agent::SandboxRecord>,
    digest_matches: Option<bool>,
) -> Vec<PathGrantView> {
    let dropped_for = |path: &str| {
        sandbox.and_then(|sb| {
            sb.dropped
                .iter()
                .find(|d| d.verb == "spawn" && d.path == path)
        })
    };
    let mut rows: Vec<PathGrantView> = dirs
        .iter()
        .map(|raw| {
            let expanded = mur_agent_runtime::sandbox::policy::expand_entitlement_path(raw)
                .display()
                .to_string();
            let status = match (sandbox, dropped_for(&expanded)) {
                (None, _) => GrantStatus::Unverified,
                (Some(_), Some(d)) => GrantStatus::Dropped {
                    reason: d.reason.clone(),
                },
                (Some(_), None) if digest_matches == Some(true) => GrantStatus::Effective,
                (Some(_), None) => GrantStatus::Unverified,
            };
            PathGrantView {
                raw: raw.clone(),
                expanded,
                status,
            }
        })
        .collect();

    if let Some(sb) = sandbox {
        for d in sb.dropped.iter().filter(|d| d.verb == "spawn") {
            if rows.iter().any(|r| r.expanded == d.path) {
                continue;
            }
            rows.push(PathGrantView {
                raw: d.path.clone(),
                expanded: d.path.clone(),
                status: GrantStatus::Dropped {
                    reason: d.reason.clone(),
                },
            });
        }
    }
    rows
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
    let spawn_dirs = &ent.processes.spawn.allowed_dirs;
    // `Some(false)` only when the seal recorded a digest and it still matches.
    // A lock sealed before the digest existed (`None`) cannot say whether a
    // build-lane grant reached the kernel, so it must not read as "matches".
    let spawn_dirs_match = sandbox
        .and_then(|sb| sb.granted_spawn_dirs_digest.as_deref())
        .map(|sealed| sealed == spawn_dirs_digest(spawn_dirs));
    // An override relaxes the sealed entitlements on purpose. The seal was
    // built from the profile with the override merged, so that is what its
    // digest is compared against; any other difference is still an edit.
    let applied = lock.and_then(|l| l.startup_override.as_ref());
    let sealed_fs = match applied {
        Some(r) => {
            let mut merged = ent.clone();
            mur_common::startup_override::merge::apply(&mut merged, r.flags);
            merged.filesystem
        }
        None => ent.filesystem.clone(),
    };
    let grants_drifted = sandbox
        .is_some_and(|sb| sb.granted_digest != filesystem_grants_digest(&sealed_fs))
        || spawn_dirs_match == Some(false);
    let relaxed_by_override = applied.map(|r| OverrideView {
        nonce: r.nonce.clone(),
        flags: r.flags.cli_names().into_iter().map(String::from).collect(),
    });

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
        relaxed_by_override,
        runtime_outbound: OutboundView {
            mode: out.mode,
            allow_hosts: out.allow_hosts.clone(),
            base_ports: mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS.to_vec(),
            allow_ports: out.allow_ports.clone(),
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
            dir_grants: dir_grants(spawn_dirs, sandbox, spawn_dirs_match),
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
        intent_check: profile.hitl.intent_check.clone(),
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
/// `chain` supplies the denies the SBPL emitter installs after the allows.
/// Passed in rather than resolved here so this stays a pure function: the
/// launch chain is derived from real paths on disk, and a renderer that reads
/// the filesystem cannot be tested against a fixture.
pub(super) fn paths_picture(
    name: &str,
    profile: &mur_common::AgentProfile,
    lock: Option<&LockFile>,
    chain: Option<&mur_agent_runtime::sandbox::launch_chain::LaunchChain>,
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
        if list.is_empty() && !(label == "deny" && chain.is_some()) {
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
        // The denies that do most of the work are not in the profile at all.
        // Without them a `✓` write grant on `~/.mur` reads as "every sibling's
        // profile.yaml is writable", which is what this report was believed to
        // be saying (#1198). It is not: the emitter closes the sibling tree
        // again after the allows, and SBPL is last-match-wins.
        if label == "deny"
            && let Some(chain) = chain
        {
            let _ = writeln!(
                o,
                "  launch chain — installed after the grants above, so these win"
            );
            let self_home = chain.agent_self_home().display().to_string();
            for d in chain.deny_paths() {
                let _ = writeln!(o, "    ✓ {}", d.display());
                // The one exception, and the reason the agent's own tree is
                // writable while its siblings are not.
                if d.ends_with("agents") {
                    let _ = writeln!(o, "        except {self_home} — re-allowed next");
                }
            }
        }
    }

    if !v.processes.dir_grants.is_empty() {
        let _ = writeln!(o, "\nBUILD LANE (exec)");
        for g in &v.processes.dir_grants {
            match &g.status {
                GrantStatus::Dropped { reason } => {
                    let _ = writeln!(o, "  ✗ {}\n      inactive — {reason}", g.raw);
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

    if let Some(r) = &v.relaxed_by_override {
        let _ = writeln!(
            o,
            "\nRelaxed by startup override ({}): {}. The rows above are the profile \
             as written; this boot runs with what those flags open, until a plain \
             restart:\n    mur agent status {name}",
            r.nonce,
            r.flags.join(", ")
        );
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
mod tests;
