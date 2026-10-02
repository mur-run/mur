use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entitlements {
    pub network: NetworkEntitlement,
    pub filesystem: FilesystemEntitlement,
    pub processes: ProcessesEntitlement,
    #[serde(default)]
    pub syscalls: SyscallsEntitlement,
    #[serde(default)]
    pub limits: LimitsEntitlement,
    /// LLM call permission. Default = Allowed (back-compat). Bridges set to Off
    /// so the supervisor refuses to construct an LLM client.
    #[serde(default)]
    pub llm: crate::bridge::llm_entitlement::LlmEntitlement,
    /// Per-tool allow/ask/deny policy. Empty = all tools use default (Ask).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolRule>,
    /// When `true` (the default), a sandbox apply failure is fatal: the agent
    /// refuses to start rather than running advisory-only (unconfined).
    /// Set to `false` only for development or trusted-workstation agents that
    /// intentionally run without kernel sandbox enforcement.
    #[serde(default = "default_true")]
    pub fail_closed_on_sandbox_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkEntitlement {
    pub inbound: InboundNetwork,
    pub outbound: OutboundNetwork,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InboundNetwork {
    #[serde(default)]
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboundNetwork {
    pub mode: NetworkOutboundMode,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Extra outbound TCP ports granted on top of the built-in web set
    /// (`RESTRICTED_GENERAL_PORTS`: 80/443/8080/8443). Issue #006: without
    /// this, a non-web port (ssh 2222, vite 5173, ollama 11434) was
    /// unreachable under `restricted` and the only escape was
    /// `unrestricted`, which opens EVERY port.
    ///
    /// Honored under `Restricted` ONLY. `Off` stays air-gapped and
    /// `ProxyOnly` keeps denying general TCP — a stale entry in a profile
    /// whose mode was later tightened must never silently reopen it.
    ///
    /// This is a PORT grant, not a host grant: like the base set, the port
    /// opens to host `*`, because macOS SBPL's `remote tcp` accepts only
    /// `*` or `localhost` as the host. Bounding WHICH host is reached on
    /// that port remains HostGuard's job via `allow_hosts`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_ports: Vec<u16>,
    #[serde(default = "default_protocols")]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub resolve_dns: ResolveDnsConfig,
}
fn default_protocols() -> Vec<String> {
    vec!["tcp".to_string()]
}

/// Record of who authorized a broad egress grant, and when. Attached to a
/// per-MCP-server `McpServerNetwork` when its mode is `BroadAudited`, so the
/// grant is persisted, portable, and re-approvable on import.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EgressAuthorization {
    pub authorized_by: String,
    pub authorized_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkOutboundMode {
    Unrestricted,
    Restricted,
    /// Deny all general outbound TCP; egress is ONLY via loopback proxies
    /// (the agent's cc-proxy LLM port + the egress proxy). Hostnames are still
    /// governed by `allow_hosts` (HostGuard) — unlike `Off`, which blocks all.
    ProxyOnly,
    Off,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolveDnsConfig {
    #[serde(default = "default_dns_mode")]
    pub mode: String,
    #[serde(default)]
    pub servers: Vec<String>,
}
impl Default for ResolveDnsConfig {
    fn default() -> Self {
        Self {
            mode: default_dns_mode(),
            servers: vec![],
        }
    }
}
fn default_dns_mode() -> String {
    "system".to_string()
}

/// Dirs under `<mur_home>` where MUR objects are authored.
///
/// The seeded concierge gets read+write on these; without them the one agent a
/// fresh host has can describe a skill or workflow but cannot create one, and
/// every answer ends in "run this command yourself".
///
/// Deliberately excludes `agents/`: `self_protected()` only covers an agent's
/// OWN `profile.yaml` + `identity.key`, so write access there would let an
/// agent author a sibling with unrestricted entitlements and start it, and
/// read access would expose every other agent's Ed25519 signing key.
///
/// Deliberately excludes [`crate::paths::FLEETS`] for the same reason one
/// level up: `fleet.yaml` names a fleet's members, limits and HITL
/// pre-approvals, and `.stopped` is the operator's kill-switch. An agent that
/// can write there can widen what a `fleet_run` it triggers is allowed to do,
/// or clear the stop on it. Fleets are created with `mur fleet create`; the
/// runtime already reads `fleets/` on its own (sandbox policy), so dropping
/// the grant costs the concierge nothing it needs to *use* a fleet.
pub const AUTHORING_DIRS: [&str; 3] = ["skills", "workflows", "artifacts"];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FilesystemEntitlement {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessesEntitlement {
    pub spawn: SpawnEntitlement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpawnEntitlement {
    pub mode: SpawnMode,
    #[serde(default)]
    pub allowed: Vec<String>,
    /// Directories whose entire subtree may be exec'd — the "build lane".
    ///
    /// `allowed` cannot express a toolchain that compiles its own
    /// executables: a Rust build execs `target/debug/build/<crate>-<hash>/
    /// build-script-build`, proc-macro shims, and freshly linked test
    /// binaries, all at paths that do not exist until the build creates them
    /// and change on every dependency bump. Without this an agent granted
    /// `cargo` could compile nothing and could never verify its own work.
    ///
    /// Grant narrowly — a build-output directory, not a source tree or a
    /// home directory. Everything under it becomes exec'able, so the tree
    /// should be one the agent already has write access to and nothing else
    /// depends on. Filesystem and network entitlements still bound what the
    /// executed code can reach.
    #[serde(default)]
    pub allowed_dirs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpawnMode {
    Allowlist,
    Any,
    None,
    /// Shell-only: fences the system exec paths (`/bin`, `/usr/bin`,
    /// `/usr/lib`) that `Allowlist` mode exempts by default, so only the
    /// resolved shell binary the `bash` tool itself spawns plus the
    /// profile's own `spawn_allowed_paths`/`spawn_allowed_prefixes` may be
    /// exec'd -- no other system binary (coreutils, `git`, etc.) is implied.
    Strict,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyscallsEntitlement {
    #[serde(default = "default_syscalls_mode")]
    pub mode: String,
    #[serde(default)]
    pub extra_deny: Vec<String>,
}
fn default_syscalls_mode() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LimitsEntitlement {
    #[serde(default)]
    pub cpu_seconds: Option<u64>,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u64,
    #[serde(default = "default_fds")]
    pub file_descriptors: u32,
    #[serde(default = "default_procs")]
    pub processes: u32,
}
fn default_memory_mb() -> u64 {
    512
}
fn default_fds() -> u32 {
    1024
}
fn default_procs() -> u32 {
    32
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolPolicy {
    Allow,
    #[default]
    Ask,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRule {
    pub pattern: String,
    pub policy: ToolPolicy,
    /// Intrinsic risk tier of this tool (v3c). Resolved most-restrictive-wins
    /// against per-step risk + channel policy; gates pre-execution when not Read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<crate::hitl::RiskTier>,
}

/// Resolve the effective policy for `tool_name` against an ordered rule list.
///
/// Precedence: exact-name match > longest-prefix glob (trailing `*`) > default (`Ask`).
pub fn resolve_tool_policy(rules: &[ToolRule], tool_name: &str) -> ToolPolicy {
    resolve_tool_policy_opt(rules, tool_name).unwrap_or_default()
}

/// Like [`resolve_tool_policy`] but distinguishes "no rule matched" (`None`)
/// from an explicit rule — for tools whose registration is already gated
/// elsewhere (e.g. `fleet_run`'s config allowlist) and that therefore want a
/// different default than `Ask` while still honoring explicit rules.
pub fn resolve_tool_policy_opt(rules: &[ToolRule], tool_name: &str) -> Option<ToolPolicy> {
    for rule in rules {
        if rule.pattern == tool_name {
            return Some(rule.policy);
        }
    }
    let mut best: Option<(&ToolRule, usize)> = None;
    for rule in rules {
        if let Some(prefix) = rule.pattern.strip_suffix('*')
            && tool_name.starts_with(prefix)
        {
            let len = prefix.len();
            if best.is_none_or(|(_, best_len)| len > best_len) {
                best = Some((rule, len));
            }
        }
    }
    best.map(|(rule, _)| rule.policy)
}

#[cfg(test)]
mod tool_policy_tests {
    use super::*;

    fn rules() -> Vec<ToolRule> {
        vec![
            ToolRule {
                pattern: "mcp__github__merge_pr".into(),
                policy: ToolPolicy::Ask,
                risk: None,
            },
            ToolRule {
                pattern: "mcp__github__*".into(),
                policy: ToolPolicy::Allow,
                risk: None,
            },
            ToolRule {
                pattern: "mcp__*".into(),
                policy: ToolPolicy::Deny,
                risk: None,
            },
            ToolRule {
                pattern: "bash".into(),
                policy: ToolPolicy::Allow,
                risk: None,
            },
        ]
    }

    #[test]
    fn exact_beats_glob() {
        assert_eq!(
            resolve_tool_policy(&rules(), "mcp__github__merge_pr"),
            ToolPolicy::Ask
        );
    }

    #[test]
    fn longer_glob_wins() {
        assert_eq!(
            resolve_tool_policy(&rules(), "mcp__github__create_issue"),
            ToolPolicy::Allow
        );
    }

    #[test]
    fn shorter_glob_fallback() {
        assert_eq!(
            resolve_tool_policy(&rules(), "mcp__slack__send"),
            ToolPolicy::Deny
        );
    }

    #[test]
    fn exact_bash() {
        assert_eq!(resolve_tool_policy(&rules(), "bash"), ToolPolicy::Allow);
    }

    #[test]
    fn unknown_tool_defaults_ask() {
        assert_eq!(
            resolve_tool_policy(&rules(), "unknown_tool"),
            ToolPolicy::Ask
        );
    }

    #[test]
    fn empty_rules_defaults_ask() {
        assert_eq!(resolve_tool_policy(&[], "bash"), ToolPolicy::Ask);
    }

    fn minimal_entitlements_yaml() -> &'static str {
        "network:\n  inbound: {}\n  outbound:\n    mode: off\nfilesystem: {}\nprocesses:\n  spawn:\n    mode: none\n"
    }

    #[test]
    fn entitlements_tools_defaults_empty() {
        let e: Entitlements = serde_yaml_ng::from_str(minimal_entitlements_yaml()).unwrap();
        assert!(e.tools.is_empty());
    }

    #[test]
    fn entitlements_tools_roundtrip() {
        let base = minimal_entitlements_yaml();
        let yaml = format!("{base}tools:\n  - pattern: \"mcp__github__*\"\n    policy: allow\n");
        let e: Entitlements = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(e.tools.len(), 1);
        assert_eq!(e.tools[0].policy, ToolPolicy::Allow);
        let y = serde_yaml_ng::to_string(&e).unwrap();
        let back: Entitlements = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(back.tools.len(), 1);
        assert_eq!(back.tools[0].policy, ToolPolicy::Allow);
    }
    #[test]
    fn denylist_membership_and_mutation() {
        let mut list: Vec<String> = vec![];
        assert!(name_enabled(&list, "a"), "empty denylist => enabled");

        set_denylist(&mut list, "a", false); // disable
        assert!(!name_enabled(&list, "a"));
        assert_eq!(list, ["a"]);

        set_denylist(&mut list, "a", false); // idempotent disable
        assert_eq!(list, ["a"], "no duplicate entries");

        set_denylist(&mut list, "a", true); // enable removes
        assert!(name_enabled(&list, "a"));
        assert!(list.is_empty());

        set_denylist(&mut list, "b", true); // enabling an absent name is a no-op
        assert!(list.is_empty());
    }

    #[test]
    fn addon_group_rule_truth_table() {
        let mut p = crate::agent::AgentProfile::default_for_tests();
        p.addons.push(AddonRef {
            id: "grp".into(),
            source: "claude-local:grp@1.0.0".into(),
            enabled: false,
            skills: vec!["g_skill".into()],
            mcp: vec!["g_mcp".into()],
            commands: vec!["g_cmd".into()],
            content_hash: None,
            fetch_ref: None,
            fetch_plugin: None,
        });

        // 1. standalone item, no entry anywhere => enabled (back-compat)
        assert!(p.skill_enabled("standalone"));
        assert!(p.mcp_enabled("standalone_mcp"));

        // 2. grouped item, group disabled => off (cannot enable one member of a disabled group)
        assert!(!p.skill_enabled("g_skill"));
        assert!(!p.mcp_enabled("g_mcp"));

        // 3. grouped item, group enabled, name not denied => on
        assert!(p.set_addon_enabled("grp", true));
        assert!(p.skill_enabled("g_skill"));
        assert!(p.mcp_enabled("g_mcp"));

        // 4. name in denylist overrides an enabled group => off (silence one member)
        p.set_skill_enabled("g_skill", false);
        assert!(!p.skill_enabled("g_skill"));

        // set_addon_enabled on a missing id reports false
        assert!(!p.set_addon_enabled("nope", true));

        // kill-switch: only flips group flags — no denylist push
        p.disable_all_addons();
        assert!(p.addons.iter().all(|g| !g.enabled));
        assert!(!p.skill_enabled("g_skill"));
        assert!(!p.skill_enabled("g_cmd"));
        assert!(!p.mcp_enabled("g_mcp")); // mcp kill-switch asserted

        // re-enable restores members — kill-switch is NOT sticky
        // (g_skill was individually denied in step 4 above and stays off;
        //  g_cmd and g_mcp were never individually denied so they come back on)
        assert!(p.set_addon_enabled("grp", true));
        assert!(!p.skill_enabled("g_skill")); // still individually denied from step 4
        assert!(p.skill_enabled("g_cmd")); // restored: never individually denied
        assert!(p.mcp_enabled("g_mcp")); // restored: never individually denied

        // clearing the individual deny fully restores g_skill too
        p.set_skill_enabled("g_skill", true);
        assert!(p.skill_enabled("g_skill"));
    }

    #[test]
    fn addon_ref_content_hash_and_fetch_ref_default_none_and_round_trip() {
        // legacy AddonRef (no new fields) → None
        let legacy = "id: a\nsource: claude-local:a@1\nenabled: false\n";
        let r: AddonRef = serde_yaml_ng::from_str(legacy).unwrap();
        assert_eq!(r.content_hash, None);
        assert_eq!(r.fetch_ref, None);

        // with the new fields → round-trips
        let full = "id: a\nsource: claude-local:a@1\nenabled: true\ncontent_hash: abc123\nfetch_ref: owner/repo\n";
        let r2: AddonRef = serde_yaml_ng::from_str(full).unwrap();
        assert_eq!(r2.content_hash.as_deref(), Some("abc123"));
        assert_eq!(r2.fetch_ref.as_deref(), Some("owner/repo"));
        let back = serde_yaml_ng::to_string(&r2).unwrap();
        let r3: AddonRef = serde_yaml_ng::from_str(&back).unwrap();
        assert_eq!(r2, r3);
    }
}
