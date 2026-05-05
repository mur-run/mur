//! Agent profile, Agent Card, and LockFile types shared between
//! mur-agent-runtime and mur-core.

use crate::companion::{Formality, Relationship};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentProfile {
    pub schema: u32,
    pub id: String, // UUIDv7
    pub name: String,
    pub display_name: String,
    pub version: String,
    pub persona: Persona,
    pub sys_prompt_file: String,
    pub model: ModelConfig,
    /// Optional pointer into ~/.mur/models.yaml. When set, the runtime
    /// prefers the registry entry over the inline `model:` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,
    #[serde(default)]
    pub skills: Vec<String>,
    pub transport: TransportConfig,
    pub communication: CommunicationConfig,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub entitlements: Entitlements,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    pub retry: RetryConfig,
    pub lifecycle: LifecycleConfig,
    /// Cryptographic identity for cross-host A2A (P0a.5+). Default = empty
    /// (legacy P0a profiles continue to load without this block).
    #[serde(default)]
    pub identity: IdentityConfig,
    #[serde(default)]
    pub file_transfer: FileTransferConfig,
    #[serde(default)]
    pub deployment: DeploymentConfig,
    /// Companion subsystem (Phase 1.1+). Default = disabled (legacy profiles
    /// continue to load without this block).
    #[serde(default)]
    pub companion: CompanionConfig,
    /// Pubkeys of bridges (and other LLM-less peers) this agent will accept
    /// signed envelopes from. Empty = accept no bridge traffic. Default = empty.
    #[serde(default)]
    pub trusted_peers: Vec<crate::bridge::peer::TrustedPeer>,
    pub created_at: String,
    pub updated_at: String,
}

fn default_algorithm() -> String {
    "ed25519".into()
}

/// Algorithms the runtime can generate + verify.
pub const SUPPORTED_ALGORITHMS: &[&str] = &["ed25519"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdentityConfig {
    /// Multibase-encoded Ed25519 public key (base58btc, `z` prefix).
    /// Empty string for legacy P0a profiles; filled on P0a.5 `mur agent create`.
    #[serde(default)]
    pub pubkey: String,
    /// Free-form owner identity (email / SSO sub). None for legacy profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    // P0a.6 rekey extensions (all #[serde(default)] — back-compat)
    /// Cryptographic algorithm for this key. Defaults to "ed25519".
    #[serde(default = "default_algorithm")]
    pub algorithm: String,
    /// Monotonic version counter; 0 = initial create, increments on each rotation.
    #[serde(default)]
    pub key_version: u32,
    /// RFC3339 timestamp of when this key was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_key: Option<String>,
    /// Previous public key (before most recent rotation). None if not rotated yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_pubkey: Option<String>,
    /// Version of the previous key. None if not rotated yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_key_version: Option<u32>,
    /// RFC3339 timestamp when grace period expires and old key is fully retired.
    /// Only set during rotation; cleared once grace period ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_expires_at: Option<String>,
    /// RFC3339 timestamp of the most recent key rotation (normal, not emergency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_at: Option<String>,
    /// RFC3339 timestamp of emergency key rotation (set only if emergency rekey occurred).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emergency_rekey_at: Option<String>,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            pubkey: String::new(),
            owner: None,
            algorithm: default_algorithm(),
            key_version: 0,
            created_at_key: None,
            previous_pubkey: None,
            previous_key_version: None,
            grace_expires_at: None,
            rotated_at: None,
            emergency_rekey_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Persona {
    pub category: PersonaCategory,
    pub description: String,
    pub traits: PersonaTraits,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PersonaCategory {
    Research,
    Automation,
    Monitor,
    Notify,
    Commerce,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonaTraits {
    pub tone: String,
    pub risk: String,
    pub verbosity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    pub provider: String,
    pub name: String,
    #[serde(default)]
    pub params: BTreeMap<String, serde_yaml_ng::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerEntry {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransportConfig {
    pub stdio: bool,
    pub socket: SocketTransportConfig,
    #[serde(default)]
    pub tcp: TcpTransportConfig,
    /// Track C5 — HTTP webhook receiver. Default off; enabling
    /// requires an HMAC secret in the OS keychain (`SecretRef`).
    /// See `docs/superpowers/specs/2026-05-05-mur-agent-c5-webhook-design.md`.
    #[serde(default)]
    pub webhook: WebhookTransportConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TcpTransportConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bind: String,
    #[serde(default)]
    pub noise: NoiseConfig,
}

/// HTTP webhook receiver — Track C5.
///
/// External systems POST `SharePayload`-shaped JSON to
/// `http://<bind>:<port>/agents/<slug>/webhook` with an
/// `X-Mur-Signature: sha256=<hex>` header carrying an HMAC-SHA256
/// over the raw body. The HMAC secret is stored in the OS keychain
/// via `SecretRef` (same pattern as Telegram bot tokens in C2);
/// `hmac_secret_ref` is the `service:account` lookup key.
///
/// `bind` defaults to `127.0.0.1` so a fresh enable doesn't
/// inadvertently expose the agent to the local network. Users who
/// want VPN / Tailscale reachability override to `0.0.0.0` or the
/// VPN interface address explicitly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookTransportConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_webhook_bind")]
    pub bind: String,
    #[serde(default = "default_webhook_port")]
    pub port: u16,
    /// `service:account` key into the OS keychain. Empty string
    /// when `enabled = false`; required (and validated) at startup
    /// when enabled.
    #[serde(default)]
    pub hmac_secret_ref: String,
}

fn default_webhook_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_webhook_port() -> u16 {
    6789
}

impl Default for WebhookTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_webhook_bind(),
            port: default_webhook_port(),
            hmac_secret_ref: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoiseConfig {
    pub pattern: String,
}

impl Default for NoiseConfig {
    fn default() -> Self {
        Self {
            pattern: "Noise_XK_25519_ChaChaPoly_BLAKE2s".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SocketTransportConfig {
    pub enabled: bool,
    pub bind: String, // "unix:///path" or "tcp://host:port" (P0b)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthConfig {
    pub scheme: String,
    pub token_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommunicationConfig {
    #[serde(default = "default_accepts_all")]
    pub accepts_from: Vec<String>,
    #[serde(default)]
    pub sends_to: Vec<String>,
}
fn default_accepts_all() -> Vec<String> {
    vec!["*".to_string()]
}

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
    #[serde(default = "default_protocols")]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub resolve_dns: ResolveDnsConfig,
}
fn default_protocols() -> Vec<String> {
    vec!["tcp".to_string()]
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkOutboundMode {
    Unrestricted,
    Restricted,
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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpawnMode {
    Allowlist,
    Any,
    None,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleEntry {
    pub cron: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends_to: Option<String>,
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

// ──────────────────────────────────────────────────────────────────────────
// Companion subsystem (Phase 1.1+) — see
// docs/superpowers/specs/2026-04-29-mur-companion-phase-1-1-design.md §3.1
// ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompanionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default)]
    pub relationship: Relationship,
    #[serde(default)]
    pub voice_overrides: VoiceOverrides,
    #[serde(default)]
    pub onboarding: OnboardingState,
    #[serde(default)]
    pub rhythm: RhythmConfig,
    #[serde(default)]
    pub proactive: ProactiveConfig,
}

/// Resolve a default BCP-47 locale from the `LANG` environment variable
/// (e.g. `zh_TW.UTF-8` → `zh-TW`). Falls back to `en-US`.
pub fn default_locale() -> String {
    std::env::var("LANG")
        .ok()
        .and_then(|v| v.split('.').next().map(|s| s.replace('_', "-")))
        .unwrap_or_else(|| "en-US".into())
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_for_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formality: Option<Formality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirstMemory {
    pub text: String,
    pub established_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnboardingState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_memory: Option<FirstMemory>,
}

/// Phase 1.2 reservation. 1.1 keeps `enabled = false` (rhythm collection is
/// out of 1.1 scope).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RhythmConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProactiveConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 1.1 reserves the field; 1.2 will write `now + 7d` at rhythm-enable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<QuietHours>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_hours: Option<ActiveHours>,
    #[serde(default = "default_daily_cap")]
    pub daily_cap: u8,
    #[serde(default = "default_channels")]
    pub channels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for ProactiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            learning_until: None,
            quiet_hours: None,
            active_hours: None,
            daily_cap: default_daily_cap(),
            channels: default_channels(),
            paused_until: None,
        }
    }
}

fn default_daily_cap() -> u8 {
    3
}
fn default_channels() -> Vec<String> {
    vec!["stdout".into()]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuietHours {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveHours {
    pub start: String,
    pub end: String,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

#[cfg(test)]
mod model_ref_tests {
    use super::*;

    #[test]
    fn legacy_profile_without_model_ref_still_parses() {
        let yaml = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            p.model_ref.is_none(),
            "legacy profile must not have model_ref"
        );
    }

    #[test]
    fn round_trip_with_model_ref_preserves_field() {
        let yaml = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let mut p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        p.model_ref = Some("anthropic_opus_4_7".into());
        let s = serde_yaml_ng::to_string(&p).unwrap();
        assert!(s.contains("model_ref: anthropic_opus_4_7"), "yaml: {s}");
        let p2: AgentProfile = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(p2.model_ref.as_deref(), Some("anthropic_opus_4_7"));
    }
}

/// GUI-facing reification of the companion's three-layer permission toggle.
///
/// On-disk schema doesn't change — this helper just maps between the
/// three independent booleans (`enabled`, `rhythm.enabled`,
/// `proactive.enabled`) and a single ordered tier. Use
/// [`ProactiveTier::from_config`] to read and [`ProactiveTier::apply`]
/// to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProactiveTier {
    Off,
    WarmOnly,
    WarmAndBehavior,
    All,
}

impl ProactiveTier {
    pub fn from_config(c: &CompanionConfig) -> Self {
        match (c.enabled, c.rhythm.enabled, c.proactive.enabled) {
            (false, _, _) => Self::Off,
            (true, false, false) => Self::WarmOnly,
            (true, true, false) => Self::WarmAndBehavior,
            (true, _, true) => Self::All,
        }
    }

    pub fn apply(&self, c: &mut CompanionConfig) {
        match self {
            Self::Off => {
                c.enabled = false;
                c.rhythm.enabled = false;
                c.proactive.enabled = false;
            }
            Self::WarmOnly => {
                c.enabled = true;
                c.rhythm.enabled = false;
                c.proactive.enabled = false;
            }
            Self::WarmAndBehavior => {
                c.enabled = true;
                c.rhythm.enabled = true;
                c.proactive.enabled = false;
            }
            Self::All => {
                c.enabled = true;
                c.rhythm.enabled = true;
                c.proactive.enabled = true;
            }
        }
    }
}
