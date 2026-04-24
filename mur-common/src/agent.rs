//! Agent profile, Agent Card, and LockFile types shared between
//! mur-agent-runtime and mur-core.

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
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct IdentityConfig {
    /// Multibase-encoded Ed25519 public key (base58btc, `z` prefix).
    /// Empty string for legacy P0a profiles; filled on P0a.5 `mur agent create`.
    #[serde(default)]
    pub pubkey: String,
    /// Free-form owner identity (email / SSO sub). None for legacy profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
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

fn default_accept_max() -> u64 { 10_485_760 }
fn default_accept_total() -> u64 { 104_857_600 }
fn default_approval_threshold() -> u64 { 10_485_760 }
fn default_reject_paths() -> Vec<String> {
    vec!["~/.ssh".into(), "~/.aws".into(), "~/.gnupg".into()]
}
fn default_allowed_mime() -> Vec<String> { vec!["*".into()] }

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

fn default_env() -> Option<String> { Some("dev".into()) }

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
