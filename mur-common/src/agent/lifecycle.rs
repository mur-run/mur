use super::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NotificationsConfig {
    #[serde(default)]
    pub on_task_complete: Vec<NotificationTarget>,
    #[serde(default)]
    pub on_error: Vec<NotificationTarget>,
    #[serde(default)]
    pub on_shutdown: Vec<NotificationTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "target", rename_all = "lowercase")]
pub enum NotificationTarget {
    Agent {
        name: String,
    },
    Commander,
    Email {
        address: String,
        #[serde(default)]
        smtp_config_file: Option<String>,
    },
    Slack {
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        webhook_url_env: Option<String>,
    },
    Webpush {
        url: String,
    },
    Webhook {
        url: String,
        #[serde(default = "default_post")]
        method: String,
        #[serde(default)]
        auth: Option<String>,
    },
}
fn default_post() -> String {
    "POST".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetryConfig {
    pub llm: RetryPolicy,
    pub tool: RetryPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub backoff: BackoffStrategy,
    pub initial_delay_ms: u64,
    #[serde(default)]
    pub max_delay_ms: Option<u64>,
    #[serde(default)]
    pub retry_on: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackoffStrategy {
    Linear,
    Exponential,
    Fixed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LifecycleConfig {
    pub restart: RestartPolicy,
    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,
    #[serde(default = "default_window")]
    pub restart_window_secs: u64,
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_secs: u64,
    #[serde(default = "default_mcp_required")]
    pub mcp_required: bool,
    #[serde(default)]
    pub execution: ExecutionMode,
    #[serde(default)]
    pub schedule: Vec<ScheduleEntry>,
    #[serde(default)]
    pub idle_triggers: Vec<IdleTrigger>,
}
fn default_max_restarts() -> u32 {
    3
}
fn default_window() -> u64 {
    600
}
fn default_stop_timeout() -> u64 {
    15
}
fn default_mcp_required() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    Daemon,
    OnDemand,
}

/// Where an agent leaves a schedule it wants but cannot create.
///
/// An agent's schedules live in `lifecycle.schedule` inside its own
/// `profile.yaml`, and an agent may not write that file — the sandbox denies it
/// unconditionally so a running agent cannot widen its own entitlements and
/// restart into them. So "remind me at 10 tomorrow" cannot become a schedule
/// from the inside, however much the agent understands the request.
///
/// It becomes a proposal instead: a file in the agent's own home, which it may
/// write, that `mur agent schedule accept` turns into the real entry.
///
/// Public and shared because both halves must name the same directory. Two
/// spellings would not fail loudly — the agent would write proposals nobody
/// lists, which is the shape of failure this whole area keeps producing.
pub const SCHEDULE_PROPOSAL_DIR: &str = "schedule-proposals";

/// File in the agent's home holding the id of the channel a fired schedule
/// leaves its reply in. One stable channel per agent, remembered rather than
/// re-derived (#1125).
pub const SCHEDULE_CHANNEL_FILE: &str = "schedule-channel";

/// Marker file in the agent's home naming the channel that records chat-gate
/// decisions (`HitlResponse` events keyed by `action_hash`). Same shape as
/// `SCHEDULE_CHANNEL_FILE`: created on first use, replaced if it names a
/// channel that no longer loads.
pub const HITL_CHANNEL_FILE: &str = "hitl-channel";

/// A schedule an agent asked for and a person has not yet granted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleProposal {
    pub cron: String,
    pub message: String,
    /// What the user actually said, kept verbatim: a cron expression is not
    /// reviewable on its own, and the reviewer is being asked whether this is
    /// what they meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_for: Option<String>,
    /// Proposed bound, carried verbatim onto the accepted [`ScheduleEntry`].
    /// Present exactly when the agent judged the request to name one occasion
    /// rather than a recurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<String>,
    pub proposed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleEntry {
    pub cron: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends_to: Option<String>,
    /// Retire the entry once its next firing would fall after this instant
    /// (RFC3339 with offset). How a one-shot reminder is expressed: cron has no
    /// year field, so "tomorrow at 10:00" can only be written as an annual
    /// recurrence, and unbounded it turns a request for one morning into a
    /// perpetual commitment (#1119).
    ///
    /// A bound rather than a fired-yet flag, because the scheduler runs inside
    /// the agent's own sandbox where `profile.yaml` is denied
    /// (`SELF_PROTECTED_AGENT_FILES`, #712) — it cannot record that an entry has
    /// fired. Comparing the next firing against a stored instant needs no write
    /// at all, so the bound works where a flag structurally could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdleTrigger {
    /// Idle threshold in seconds. Fires when (now - last_activity) >= after_secs.
    pub after_secs: u64,
    /// Message body injected into the task runner when this trigger fires.
    pub message: String,
    /// Optional A2A peer to route the resulting reply to. None means the agent itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends_to: Option<String>,
    /// Per-trigger refire cooldown in seconds. Prevents tight loops when the
    /// idle threshold is short and the runner finishes quickly. Default 600.
    #[serde(default = "default_idle_cooldown")]
    pub cooldown_secs: u64,
    /// When true, suppress firing during the agent's quiet-hours window.
    /// Default true — idle pings should not wake the user at 3 a.m.
    #[serde(default = "default_true")]
    pub respect_quiet_hours: bool,
}

fn default_idle_cooldown() -> u64 {
    600
}
/// True if `name` is not present in a denylist (i.e. enabled).
pub fn name_enabled(denylist: &[String], name: &str) -> bool {
    !denylist.iter().any(|n| n == name)
}

/// Add/remove `name` in a denylist. `enabled=true` removes it (idempotent),
/// `enabled=false` adds it once (idempotent).
pub fn set_denylist(list: &mut Vec<String>, name: &str, enabled: bool) {
    if enabled {
        list.retain(|n| n != name);
    } else if !list.iter().any(|n| n == name) {
        list.push(name.to_string());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileTransferConfig {
    #[serde(default = "default_accept_max")]
    pub accept_incoming_file_max_bytes: u64,
    #[serde(default = "default_accept_total")]
    pub accept_incoming_total_per_hour: u64,
    #[serde(default = "default_approval_threshold")]
    pub require_approval_above_bytes: u64,
    #[serde(default = "default_reject_paths")]
    pub reject_paths: Vec<String>,
    #[serde(default = "default_allowed_mime")]
    pub allowed_mime_types: Vec<String>,
}

impl Default for FileTransferConfig {
    fn default() -> Self {
        Self {
            accept_incoming_file_max_bytes: default_accept_max(),
            accept_incoming_total_per_hour: default_accept_total(),
            require_approval_above_bytes: default_approval_threshold(),
            reject_paths: default_reject_paths(),
            allowed_mime_types: default_allowed_mime(),
        }
    }
}

fn default_accept_max() -> u64 {
    10_485_760
}
fn default_accept_total() -> u64 {
    104_857_600
}
fn default_approval_threshold() -> u64 {
    10_485_760
}
fn default_reject_paths() -> Vec<String> {
    vec!["~/.ssh".into(), "~/.aws".into(), "~/.gnupg".into()]
}
fn default_allowed_mime() -> Vec<String> {
    vec!["*".into()]
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentType {
    #[default]
    Laptop,
    Vm,
    Docker,
    K8s,
    Lambda,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeploymentConfig {
    #[serde(rename = "type", default)]
    pub deployment_type: DeploymentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default = "default_env")]
    pub environment: Option<String>,
}

impl Default for DeploymentConfig {
    fn default() -> Self {
        Self {
            deployment_type: DeploymentType::default(),
            region: None,
            environment: default_env(),
        }
    }
}

fn default_env() -> Option<String> {
    Some("dev".into())
}

/// One filesystem grant the sandbox refused to install, and why.
///
/// The grant stays in `profile.yaml` — this records that it did not reach the
/// kernel, which is otherwise knowable only from a WARN line in a log nobody
/// queries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DroppedGrant {
    pub path: String,
    /// `"read"` or `"write"`.
    pub verb: String,
    pub reason: String,
}

/// Digest of the filesystem half of a profile's entitlements.
///
/// Narrower than `card_digest` on purpose: that one moves whenever any profile
/// field does, so using it to flag "grants changed since this agent started"
/// would raise a false alarm on an unrelated edit — and a status line that
/// cries wolf is one people stop reading.
pub fn filesystem_grants_digest(fs: &FilesystemEntitlement) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for (label, list) in [("r", &fs.read), ("w", &fs.write), ("d", &fs.deny)] {
        let mut sorted = list.clone();
        sorted.sort();
        for p in sorted {
            h.update(label.as_bytes());
            h.update(b"\0");
            h.update(p.as_bytes());
            h.update(b"\0");
        }
    }
    format!("sha256:{:x}", h.finalize())
}

/// What the sandbox actually installed, recorded at the moment it sealed.
///
/// A seatbelt profile cannot be widened after `sandbox_init`, so this is fixed
/// for the process's lifetime — the same lifetime as the lock file it rides in.
/// Without it, `profile.yaml` is the only readable account of an agent's
/// permissions, and it describes what was asked for rather than what took
/// effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SandboxRecord {
    /// False means the kernel sandbox is NOT installed and only advisory hooks
    /// remain — the agent then has MORE access than its profile grants, which
    /// is the opposite of every other failure here and the one worth shouting.
    pub enforcing: bool,
    /// `"macos-sbpl"`, `"linux-landlock"`, `"advisory-only"`, …
    pub mode: String,
    /// Digest of `entitlements.filesystem` as sealed. Comparing it against the
    /// profile on disk answers "were grants changed since this agent started"
    /// without anyone tracking that — and unlike `card_digest` it does not move
    /// when an unrelated field does, so it cannot raise a false alarm.
    pub granted_digest: String,
    /// Grants that did not reach the kernel. Empty is the normal case.
    #[serde(default)]
    pub dropped: Vec<DroppedGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockFile {
    pub schema: u32,
    pub uuid: String,
    pub name: String,
    pub pid: u32,
    pub ppid: u32,
    pub started_at: String,
    pub binary_version: String,
    pub transports: LockTransports,
    pub card_digest: String,
    pub capabilities: Vec<String>,
    /// Git sha the running binary was built from (mur_common::build::SHORT_SHA).
    /// Empty = an old lock predating this field. Drives stale detection.
    #[serde(default)]
    pub build_sha: String,
    /// A2A method-surface version this runtime supports (A2A_PROTO_VERSION).
    /// 0 = an old lock; the dial gates versioned methods on it.
    #[serde(default)]
    pub proto_version: u32,
    /// What the sandbox installed at seal time. `None` = a lock written before
    /// this field existed, or a platform that installs no sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockTransports {
    pub stdio: bool,
    #[serde(default)]
    pub unix_socket: Option<String>,
    #[serde(default)]
    pub tcp: Option<String>,
    /// C5 / M5.3 — webhook listener URL (e.g. `http://127.0.0.1:6789`).
    /// Populated by the supervisor when `transport.webhook.enabled =
    /// true` so peers and the commander can discover the live
    /// endpoint without re-reading `profile.yaml`.
    #[serde(default)]
    pub webhook: Option<String>,
}

#[cfg(test)]
mod tests {
    /// Order must not matter: the same grants written in a different order are
    /// the same grants, and a digest that disagreed would report "restart to
    /// apply" after a cosmetic profile edit.
    #[test]
    fn grants_digest_ignores_order_but_not_content() {
        let a = FilesystemEntitlement {
            read: vec!["/a".into(), "/b".into()],
            write: vec!["/w".into()],
            deny: vec![],
        };
        let reordered = FilesystemEntitlement {
            read: vec!["/b".into(), "/a".into()],
            ..a.clone()
        };
        let changed = FilesystemEntitlement {
            write: vec!["/w".into(), "/x".into()],
            ..a.clone()
        };
        assert_eq!(
            filesystem_grants_digest(&a),
            filesystem_grants_digest(&reordered)
        );
        assert_ne!(
            filesystem_grants_digest(&a),
            filesystem_grants_digest(&changed)
        );
    }

    /// A read grant and a write grant for the same path are different grants.
    #[test]
    fn grants_digest_separates_the_verbs() {
        let r = FilesystemEntitlement {
            read: vec!["/p".into()],
            write: vec![],
            deny: vec![],
        };
        let w = FilesystemEntitlement {
            read: vec![],
            write: vec!["/p".into()],
            deny: vec![],
        };
        assert_ne!(filesystem_grants_digest(&r), filesystem_grants_digest(&w));
    }

    /// A lock written before this field existed must still load — every agent
    /// running at upgrade time wrote one.
    #[test]
    fn a_lock_without_the_sandbox_block_still_deserialises() {
        let old = r#"{"schema":1,"uuid":"u","name":"n","pid":1,"ppid":0,
            "started_at":"t","binary_version":"v",
            "transports":{"stdio":true},"card_digest":"d","capabilities":[]}"#;
        let lf: LockFile = serde_json::from_str(old).expect("old lock must load");
        assert!(lf.sandbox.is_none());
    }

    #[test]
    fn the_sandbox_block_round_trips() {
        let rec = SandboxRecord {
            enforcing: false,
            mode: "advisory-only".into(),
            granted_digest: "sha256:x".into(),
            dropped: vec![DroppedGrant {
                path: "/gone".into(),
                verb: "write".into(),
                reason: "path does not exist on disk".into(),
            }],
        };
        let back: SandboxRecord =
            serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
        assert_eq!(back, rec);
    }

    use super::*;

    #[test]
    fn broad_audited_mcp_net_serde_roundtrip_and_defaults() {
        let net = McpServerNetwork {
            mode: McpNetMode::BroadAudited,
            allow_hosts: vec![],
            deny_hosts: vec!["evil.example".into()],
            authorization: Some(EgressAuthorization {
                authorized_by: "david".into(),
                authorized_at_ms: 1_750_000_000_000,
            }),
        };
        let y = serde_yaml::to_string(&net).unwrap();
        assert!(y.contains("broad_audited"));
        let back: McpServerNetwork = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, net);
        // legacy per-server policy without the new fields still parses (serde default)
        let legacy: McpServerNetwork =
            serde_yaml::from_str("mode: restricted\nallow_hosts: []\n").unwrap();
        assert_eq!(legacy.deny_hosts, Vec::<String>::new());
        assert!(legacy.authorization.is_none());
    }

    #[test]
    fn mcp_entry_network_is_optional_and_round_trips() {
        // Absent in YAML → None (every existing profile keeps working).
        let bare = "name: x\ncommand: npx\n";
        let e: McpServerEntry = serde_yaml_ng::from_str(bare).unwrap();
        assert!(e.network.is_none());

        // Present → parsed.
        let with = "name: browser\ncommand: npx\nnetwork:\n  mode: restricted\n  allow_hosts: [\"example.com\", \"*.api.example.com\"]\n";
        let e2: McpServerEntry = serde_yaml_ng::from_str(with).unwrap();
        let net = e2.network.expect("network present");
        assert_eq!(net.mode, McpNetMode::Restricted);
        assert_eq!(net.allow_hosts, vec!["example.com", "*.api.example.com"]);

        // Round-trip keeps None out of the serialized form.
        let out = serde_yaml_ng::to_string(&e).unwrap();
        assert!(!out.contains("network"));
    }

    #[test]
    fn profile_round_trip_yaml() {
        let yaml = r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPE
name: agent_a
display_name: "Price Hunter"
version: "0.1.0"
persona:
  category: research
  description: "Finds prices"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "llama3.2:3b", params: { temperature: 0.2, max_tokens: 4096 } }
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: { enabled: true, bind: "unix:///tmp/a.sock" }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: ["a2a.message.send", "a2a.tasks"]
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
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout, connection_error] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-22T10:00:00+08:00"
"#;
        let profile: AgentProfile = serde_yaml_ng::from_str(yaml).expect("parse");
        assert_eq!(profile.name, "agent_a");
        assert_eq!(profile.persona.category, PersonaCategory::Research);
        assert_eq!(
            profile.entitlements.network.outbound.mode,
            NetworkOutboundMode::Restricted
        );
        let reserialized = serde_yaml_ng::to_string(&profile).expect("emit");
        let round_tripped: AgentProfile = serde_yaml_ng::from_str(&reserialized).expect("re-parse");
        assert_eq!(profile.id, round_tripped.id);
    }

    #[test]
    fn requires_capabilities_defaults_empty_and_round_trips() {
        let base = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(base).unwrap();
        assert!(p.requires_capabilities.is_empty());
        let with = format!("{base}\nrequires_capabilities:\n  - media\n");
        let p2: AgentProfile = serde_yaml_ng::from_str(&with).unwrap();
        assert_eq!(p2.requires_capabilities, vec!["media"]);
    }
}

#[cfg(test)]
mod idle_trigger_tests {
    use super::*;

    #[test]
    fn idle_trigger_yaml_round_trip() {
        let yaml = r#"
restart: on_failure
idle_triggers:
  - after_secs: 3600
    message: "still there?"
    sends_to: other_agent
    cooldown_secs: 1800
    respect_quiet_hours: true
"#;
        let cfg: LifecycleConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(cfg.idle_triggers.len(), 1);
        assert_eq!(cfg.idle_triggers[0].after_secs, 3600);
        assert_eq!(cfg.idle_triggers[0].message, "still there?");
        assert_eq!(
            cfg.idle_triggers[0].sends_to.as_deref(),
            Some("other_agent")
        );
        assert_eq!(cfg.idle_triggers[0].cooldown_secs, 1800);
        assert!(cfg.idle_triggers[0].respect_quiet_hours);
    }

    #[test]
    fn idle_trigger_defaults_when_omitted() {
        let yaml = "restart: on_failure\n";
        let cfg: LifecycleConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(cfg.idle_triggers.is_empty());
    }
}

#[cfg(test)]
mod lockfile_compat_tests {
    use super::*;

    #[test]
    fn lockfile_new_fields_default_for_old_locks() {
        // An old lock JSON without build_sha/proto_version must still parse,
        // defaulting to "" / 0 (= "predates this feature → stale/unsupported").
        let old = r#"{"schema":1,"uuid":"u","name":"a","pid":1,"ppid":1,
          "started_at":"t","binary_version":"mur-agent-runtime 2.26.9",
          "transports":{"stdio":true},"card_digest":"d","capabilities":[]}"#;
        let lock: LockFile = serde_json::from_str(old).unwrap();
        assert_eq!(lock.build_sha, "");
        assert_eq!(lock.proto_version, 0);
    }
}
