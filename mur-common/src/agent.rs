//! Agent profile, Agent Card, and LockFile types shared between
//! mur-agent-runtime and mur-core.

use crate::companion::{Formality, Relationship};
use crate::deps::ProgramDep;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Skill metadata broadcast in the Agent Card (Layer 1 + Layer 2).
///
/// Populated by `mur skill install` (registry or agent:// URL). Distinct from
/// `AgentProfile.skills`, which is the legacy per-agent-path list managed by
/// `mur agent skill add`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SkillCardEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub publisher: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<SkillCardTrigger>,
    /// Layer 2 abstract — injected at session start (~200 tokens).
    /// On-disk YAML key is `abstract` (a Rust reserved word).
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "abstract")]
    pub abstract_text: String,
    /// Provenance chain copied from the installed manifest. Empty for
    /// registry-installed skills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transfer_chain: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SkillCardTrigger {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentProfile {
    pub schema: u32,
    pub id: String, // UUIDv7
    pub name: String,
    pub display_name: String,
    /// Coarse human-facing role for grouping/filtering (e.g. "Engineer").
    /// A free label, not a registry — bundled defaults are UI suggestions and
    /// users can type their own. Discovery/job-routing stays on the A2A card's
    /// skills/tags; this is purely organizational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub version: String,
    pub persona: Persona,
    pub sys_prompt_file: String,
    pub model: ModelConfig,
    /// Optional pointer into ~/.mur/models.yaml. When set, the runtime
    /// prefers the registry entry over the inline `model:` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
    /// Per-agent fallback chain (ordered model_refs). Overrides the global
    /// `models.fallback_chain` when non-empty. See the model-switch spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_chain: Vec<String>,
    /// Per-agent difficulty-routing override. Inherits the global
    /// `models.routing` when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<crate::config::RoutingConfig>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,
    #[serde(default)]
    pub skills: Vec<String>,
    /// Skills installed via `mur skill install`. Distinct from `skills`
    /// (which holds legacy per-agent paths from `mur agent skill add`).
    /// Broadcast in the Agent Card alongside `skills`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub installed_skills: Vec<SkillCardEntry>,
    /// Per-agent skill denylist (add-on Phase 1). Skill names that are
    /// installed/visible to this agent but suppressed from injection.
    /// Non-destructive: the skill's files/stats are untouched. Empty = all
    /// visible skills enabled (back-compat: absent in old profiles).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_skills: Vec<String>,

    /// Per-agent MCP denylist (add-on Phase 1). `McpServerEntry` names not
    /// spawned for this agent. Non-destructive: the entry + its pin stay in
    /// the profile. Empty = all configured servers enabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_mcp: Vec<String>,
    /// Plugin-groups imported by this agent (add-on Phase 2). Each is
    /// self-contained (members installed per-agent). Absent/empty in
    /// legacy profiles (back-compat).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addons: Vec<AddonRef>,
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
    /// Human-in-the-loop configuration (Phase 2). Default = disabled.
    #[serde(default)]
    pub hitl: HitlConfig,
    /// Voice I/O configuration (D1). Default = disabled.
    #[serde(default)]
    pub voice: VoiceConfig,
    /// A1: config-driven handler picker. Absent block = all defaults.
    #[serde(default)]
    pub hooks: crate::HooksConfig,
    /// Pubkeys of bridges (and other LLM-less peers) this agent will accept
    /// signed envelopes from. Empty = accept no bridge traffic. Default = empty.
    #[serde(default)]
    pub trusted_peers: Vec<crate::bridge::peer::TrustedPeer>,
    pub created_at: String,
    pub updated_at: String,
    /// Hub companion visual identity (M-h3). Default = default-blob / Normal / Pending.
    #[serde(default)]
    pub appearance: AgentAppearance,
    /// E6: Pattern federation — snapshot filter + outbox config.
    #[serde(default)]
    pub federation: FederationConfig,

    /// A1: declarative UI action list — file_actions rendered as action
    /// buttons in the pending-item selection UI. New top-level key; NOT
    /// nested under `capabilities:`.
    #[serde(default)]
    pub file_actions: Vec<crate::action::FileAction>,

    /// A2 + A3: action pipeline configuration (deletion safety + queue limits).
    #[serde(default)]
    pub action_pipeline: crate::action::ActionPipelineConfig,

    /// External programs this artifact needs at runtime (portable-deps spec).
    /// Absent → empty; resolved by `mur agent/fleet doctor` + `install-deps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_programs: Vec<ProgramDep>,

    /// Capability refs installed into this agent (Pack S3). Absent → empty;
    /// resolved against the local capability registry / bundle store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_capabilities: Vec<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct McpServerEntry {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,

    /// SHA-256 (hex, lowercase) of the binary at `command`'s resolved
    /// path, captured at install time. `None` means the entry was
    /// added before B0 M9.1 (back-compat) and rule-6 enforcement is
    /// not applied — the supervisor will warn but not block.
    /// (B0 rule 6 / M9.1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_sha256: Option<String>,

    /// SHA-256 (hex, lowercase) of the canonical-JSON of the MCP's
    /// `tools/list` response, captured at install time. `None` means
    /// the install path skipped the description probe (e.g. the MCP
    /// uses a non-stdio transport or the binary couldn't be reached)
    /// or the entry pre-dates M9. (B0 rule 6 / M9.1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_hash: Option<String>,

    /// Display-only publisher metadata captured at install time so
    /// the user can recall what they consented to. `None` for older
    /// entries. (B0 rule 6 / M9.1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<McpPublisherInfo>,

    /// RFC3339 timestamp of when the entry was added or last
    /// re-approved by the user via `mur agent mcp pin`. Used by the
    /// rug-pull dialog UX. `None` for older entries. (B0 rule 6 / M9.1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<chrono::DateTime<chrono::Utc>>,

    /// Per-tool-call timeout for this server, in seconds. `None` uses the
    /// runtime default. Slow tools (e.g. `video_analyze`: transcript fetch
    /// + local-model map-reduce) need a longer budget than the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,

    /// Per-server outbound egress override. `None` = inherit the agent-level
    /// policy (default; unchanged behavior). `Restricted` routes this server's
    /// child through the runtime egress proxy with `allow_hosts` (advisory).
    /// See `docs/superpowers/plans/2026-06-26-mcp-per-server-egress.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<McpServerNetwork>,

    /// HTTP(S) base URL for a remote (Streamable-HTTP or SSE) MCP server.
    /// Mutually exclusive with `command` in practice; `None` = stdio transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Authentication credentials for a remote MCP server.
    /// `None` = no auth (or stdio transport).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<McpAuth>,

    /// External programs this artifact needs at runtime (portable-deps spec).
    /// Absent → empty; resolved by `mur agent/fleet doctor` + `install-deps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_programs: Vec<ProgramDep>,
}

/// Authentication scheme for a remote (HTTP) MCP server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum McpAuth {
    /// Static bearer token stored as a secret reference.
    Bearer { token: crate::secret::SecretRef },
    /// OAuth 2.1 token, with dynamic client registration state.
    Oauth(OauthAuth),
}

/// OAuth 2.1 state persisted alongside remote MCP entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OauthAuth {
    /// Authorization-server token endpoint (from discovery).
    pub token_endpoint: String,
    /// Client id from dynamic client registration.
    pub client_id: String,
    /// Keychain ref to access token.
    pub access_token: crate::secret::SecretRef,
    /// Keychain ref refresh token, if server issued one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<crate::secret::SecretRef>,
    /// Unix-epoch seconds access token expires (0 = unknown).
    #[serde(default)]
    pub expires_at: u64,
}

/// How an MCP server's outbound network is scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpNetMode {
    /// Inherit the agent-level outbound policy (today's behavior). No proxy.
    #[default]
    Inherit,
    /// Allow only `allow_hosts`, routed through the runtime egress proxy.
    Restricted,
    /// Allow ALL hosts EXCEPT `deny_hosts`, routed through the runtime egress
    /// proxy, with every CONNECT audited. For trusted-but-broad tools (e.g. a
    /// web-research browser) that cannot enumerate their destinations. Requires
    /// explicit operator consent (records `authorization`); downgraded to
    /// `Inherit` on import (lowest trust). Advisory enforcement (see egress_proxy).
    BroadAudited,
    /// No outbound for this server at all.
    Off,
}

/// Env var name a sandboxed MCP child reads to self-enforce the operator's
/// `deny_hosts` overlay on connections the egress proxy cannot observe (e.g.
/// `mur-research-gateway`'s tier-2/3 browser subprocesses — the proxy only
/// sees tier-1 `reqwest` traffic). `mur-agent-runtime`'s `proxy_env_for` sets
/// this on the child's env alongside the proxy vars; a cooperating child
/// (currently `mur-research-gateway`, via `config::load`) reads it to source
/// its own deny list. Single definition shared by both crates (CLAUDE.md
/// rule 1: no duplicated literal).
pub const ENV_MCP_DENY_HOSTS: &str = "MUR_RESEARCH_DENY_HOSTS";

/// Per-MCP-server outbound egress policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct McpServerNetwork {
    #[serde(default)]
    pub mode: McpNetMode,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Deny overlay for `BroadAudited` mode: hosts blocked even though all
    /// others are allowed. Ignored by `Restricted`/`Inherit`/`Off`.
    #[serde(default)]
    pub deny_hosts: Vec<String>,
    /// Who authorized a `BroadAudited` grant, and when. `None` for other modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<EgressAuthorization>,
}

/// A plugin-group imported by one agent (add-on Phase 2). Self-contained:
/// members are installed PER-AGENT (skills under
/// `~/.mur/agents/<a>/skills/`, mcp appended to this profile's
/// `mcp_servers`). No global library, no refcounting.
///
/// Fail-closed: `enabled` defaults to `false`. Only an explicit user
/// toggle (CLI/Hub) or a trusted native installer flips it true — the
/// importer always constructs it `false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AddonRef {
    /// e.g. "superpowers" (local) or "superpowers@claude-plugins-official".
    pub id: String,
    /// Provenance, free-text. e.g. "claude-local:superpowers@6.0.3".
    pub source: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

/// Display-only publisher metadata captured at install time. None of
/// the fields are validated against any external authority — they're
/// shown to the user during the install confirm prompt and reproduced
/// in `mur agent mcp inspect` output so the user can audit who they
/// thought they were trusting. (B0 rule 6 / M9.1)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct McpPublisherInfo {
    /// Free-form publisher identifier — e.g. `"Anthropic"`,
    /// `"@github-user-alice"`, or whatever `serverInfo.name` returned.
    pub name: String,

    /// Optional homepage / docs URL. Best-effort: extracted from the
    /// MCP's `serverInfo.metadata.homepage` or registry entry when
    /// available; otherwise left unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    /// Optional registry coordinate — e.g. `"@anthropic-mcp/weather@1.2.3"`.
    /// Used purely for display; not consumed by any verification path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_id: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleEntry {
    pub cron: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends_to: Option<String>,
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

fn default_true() -> bool {
    true
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
    /// Git sha the running binary was built from (mur_common::build::SHORT_SHA).
    /// Empty = an old lock predating this field. Drives stale detection.
    #[serde(default)]
    pub build_sha: String,
    /// A2A method-surface version this runtime supports (A2A_PROTO_VERSION).
    /// 0 = an old lock; the dial gates versioned methods on it.
    #[serde(default)]
    pub proto_version: u32,
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
// Voice I/O configuration (D1 — Kokoro 82M TTS + whisper.cpp STT)
// ──────────────────────────────────────────────────────────────────────────

/// Kokoro 82M voice identity. Maps to the per-voice style vector
/// embedded in the Kokoro ONNX model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VoiceId {
    /// Default: Kokoro af_heart voice.
    #[default]
    AfHeart,
    AfBella,
    AfNicole,
    AmAdam,
    AmMichael,
}

impl VoiceId {
    /// Index into the Kokoro voices.bin style matrix (row index).
    pub fn style_index(&self) -> usize {
        match self {
            VoiceId::AfHeart => 0,
            VoiceId::AfBella => 1,
            VoiceId::AfNicole => 2,
            VoiceId::AmAdam => 3,
            VoiceId::AmMichael => 4,
        }
    }

    /// Canonical lowercase string representation (matches `FromStr` inputs).
    pub fn as_str(&self) -> &'static str {
        match self {
            VoiceId::AfHeart => "af_heart",
            VoiceId::AfBella => "af_bella",
            VoiceId::AfNicole => "af_nicole",
            VoiceId::AmAdam => "am_adam",
            VoiceId::AmMichael => "am_michael",
        }
    }
}

impl std::str::FromStr for VoiceId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "af_heart" => Ok(VoiceId::AfHeart),
            "af_bella" => Ok(VoiceId::AfBella),
            "af_nicole" => Ok(VoiceId::AfNicole),
            "am_adam" => Ok(VoiceId::AmAdam),
            "am_michael" => Ok(VoiceId::AmMichael),
            other => anyhow::bail!(
                "unknown voice ID '{other}' \
                 (valid: af_heart, af_bella, af_nicole, am_adam, am_michael)"
            ),
        }
    }
}

/// Per-agent voice I/O configuration (D1).
/// Default = disabled so existing profiles continue to load unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VoiceConfig {
    /// Whether TTS (Kokoro) + STT (whisper.cpp) are enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Kokoro voice identity for TTS output. Default: af_heart.
    #[serde(default)]
    pub voice_id: VoiceId,
    /// Optional cpal input device name for mic capture.
    /// None means the OS default input device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_device: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────
// Human-in-the-loop configuration (Phase 2)
// ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HitlConfig {
    #[serde(default = "default_hitl_timeout_secs")]
    pub timeout_secs: u32,
    /// Hard cap on agentic-loop iterations (one LLM turn + its tool calls).
    /// `None` falls back to the runner default (25). On exceeding the cap the
    /// loop exits gracefully with a summary, not a hard error.
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// Per-task ceiling on cumulative *input* tokens for the agentic loop. When
    /// crossed before a turn, the loop stops gracefully with a summary.
    /// `None` falls back to the runner default (750_000 ≈ a few dollars on
    /// Sonnet); set a lower value per profile to bound spend tightly.
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

fn default_hitl_timeout_secs() -> u32 {
    300
}

impl Default for HitlConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_hitl_timeout_secs(),
            max_iterations: None,
            max_tokens: None,
        }
    }
}

#[cfg(test)]
mod hitl_tests {
    use super::*;

    #[test]
    fn hitl_config_default_max_iterations_is_none() {
        let cfg = HitlConfig::default();
        assert!(cfg.max_iterations.is_none());
    }

    #[test]
    fn hitl_config_max_iterations_explicit() {
        let cfg: HitlConfig = serde_yaml::from_str("timeout_secs: 60\nmax_iterations: 5").unwrap();
        assert_eq!(cfg.max_iterations, Some(5));
    }

    #[test]
    fn hitl_config_default_max_tokens_is_none() {
        let cfg = HitlConfig::default();
        assert!(cfg.max_tokens.is_none());
    }

    #[test]
    fn hitl_config_max_tokens_explicit() {
        let cfg: HitlConfig = serde_yaml::from_str("timeout_secs: 60\nmax_tokens: 250000").unwrap();
        assert_eq!(cfg.max_tokens, Some(250_000));
    }
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

// ──────────────────────────────────────────────────────────────────────────
// Hub companion appearance (M-h3)
// ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentAppearance {
    /// ID of the active style preset (e.g. "chiikawa", "default-blob").
    #[serde(default = "default_style_preset")]
    pub style_preset: String,
    #[serde(default)]
    pub behavior_preset: BehaviorPreset,
    /// Required for the polaroid family; none for all others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_image_path: Option<std::path::PathBuf>,
    /// Local dir where rendered .webp expression frames are stored.
    #[serde(default = "default_expressions_dir")]
    pub expressions_dir: std::path::PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rendered_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub render_status: RenderStatus,
}

fn default_style_preset() -> String {
    "default-blob".into()
}

fn default_expressions_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("expressions")
}

impl Default for AgentAppearance {
    fn default() -> Self {
        Self {
            style_preset: default_style_preset(),
            behavior_preset: BehaviorPreset::Normal,
            source_image_path: None,
            expressions_dir: default_expressions_dir(),
            last_rendered_at: None,
            render_status: RenderStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorPreset {
    Quiet,
    #[default]
    Normal,
    Lively,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RenderStatus {
    #[default]
    Pending,
    Rendering {
        done: u8,
        total: u8,
    },
    Ready,
    Failed {
        reason: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────
// E6 — Agent Pattern Federation types
// ──────────────────────────────────────────────────────────────────────────

/// When the agent pulls an updated pattern snapshot from the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotPolicy {
    #[default]
    PullOnStart,
    PullPeriodic,
    Manual,
}

/// Filter criteria for the pattern snapshot written to the agent's patterns_cache.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PatternFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies_in: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tier: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maturity: Vec<String>,
    #[serde(default)]
    pub importance_min: f64,
    #[serde(default = "default_max_snapshot_count")]
    pub max_count: usize,
    #[serde(default)]
    pub snapshot_policy: SnapshotPolicy,
}

fn default_max_snapshot_count() -> usize {
    200
}

impl Default for PatternFilter {
    fn default() -> Self {
        Self {
            applies_in: vec![],
            tier: vec![],
            maturity: vec![],
            importance_min: 0.0,
            max_count: 200,
            snapshot_policy: SnapshotPolicy::default(),
        }
    }
}

/// Points to the knowledge-layer commit this agent's patterns_cache was built from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotRef {
    pub knowledge_commit: String,
    pub taken_at: String,
    pub filter: PatternFilter,
}

/// Federation configuration embedded in AgentProfile (E6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FederationConfig {
    #[serde(default)]
    pub filter: PatternFilter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_ref: Option<SnapshotRef>,
    #[serde(default)]
    pub evidence_flush_interval_minutes: u32,
}

impl AgentProfile {
    /// Minimal valid profile for tests — no voice, no MCP, no skills.
    ///
    /// Available in all compilation modes so integration tests in
    /// dependent crates can call it (unlike `#[cfg(test)]` items which
    /// are invisible to downstream test binaries).
    #[doc(hidden)]
    pub fn default_for_tests() -> Self {
        serde_yaml_ng::from_str(include_str!("../tests/fixtures/minimal_profile.yaml"))
            .expect("minimal profile fixture")
    }

    /// Load an agent's profile from `<mur_home>/agents/<name>/profile.yaml`.
    ///
    /// Canonical read-path counterpart to the atomic-write path used by
    /// `mur agent create`/`mur agent mcp add` (`write_atomic` in
    /// `mur-core::cmd::agent`) — callers that already have `mur_home` in
    /// hand (e.g. provisioning flows, tests) can load a profile without
    /// going through the `MUR_HOME`-env-var-based `resolve_mur_home`.
    pub fn load(mur_home: &std::path::Path, name: &str) -> anyhow::Result<Self> {
        let path = mur_home.join("agents").join(name).join("profile.yaml");
        let yaml = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        serde_yaml_ng::from_str(&yaml).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
    }

    /// The imported add-on group a skill/mcp/command name belongs to.
    pub fn group_of(&self, name: &str) -> Option<&AddonRef> {
        self.addons.iter().find(|g| {
            g.skills.iter().any(|n| n == name)
                || g.mcp.iter().any(|n| n == name)
                || g.commands.iter().any(|n| n == name)
        })
    }

    /// Whether `skill_name` is enabled (§3.3): not denied AND, if it
    /// belongs to an imported group, that group is enabled.
    pub fn skill_enabled(&self, skill_name: &str) -> bool {
        name_enabled(&self.disabled_skills, skill_name)
            && self.group_of(skill_name).is_none_or(|g| g.enabled)
    }

    /// Whether MCP server `server_id` is enabled (§3.3).
    pub fn mcp_enabled(&self, server_id: &str) -> bool {
        name_enabled(&self.disabled_mcp, server_id)
            && self.group_of(server_id).is_none_or(|g| g.enabled)
    }

    /// Toggle a skill for this agent without uninstalling it.
    pub fn set_skill_enabled(&mut self, skill_name: &str, enabled: bool) {
        set_denylist(&mut self.disabled_skills, skill_name, enabled);
    }

    /// Toggle an MCP server for this agent without removing it.
    pub fn set_mcp_enabled(&mut self, server_id: &str, enabled: bool) {
        set_denylist(&mut self.disabled_mcp, server_id, enabled);
    }

    /// Toggle an imported plugin-group as a unit. Returns false if no
    /// add-on has that id.
    pub fn set_addon_enabled(&mut self, addon_id: &str, enabled: bool) -> bool {
        match self.addons.iter_mut().find(|g| g.id == addon_id) {
            Some(g) => {
                g.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// Emergency kill-switch (§7): clears every add-on group's `enabled` flag.
    /// Members are already forced off by the group AND-gate in `skill_enabled` /
    /// `mcp_enabled`, so no denylist push is needed — and avoiding it means
    /// `set_addon_enabled(id, true)` fully restores the group without leftover
    /// per-member denials.
    pub fn disable_all_addons(&mut self) {
        for g in &mut self.addons {
            g.enabled = false;
        }
    }

    /// This agent's MCP servers minus any disabled for it.
    pub fn enabled_mcp_servers(&self) -> Vec<McpServerEntry> {
        self.mcp_servers
            .iter()
            .filter(|m| self.mcp_enabled(&m.name))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
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
        let base = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(base).unwrap();
        assert!(p.requires_capabilities.is_empty());
        let with = format!("{base}\nrequires_capabilities:\n  - media\n");
        let p2: AgentProfile = serde_yaml_ng::from_str(&with).unwrap();
        assert_eq!(p2.requires_capabilities, vec!["media"]);
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

    #[test]
    fn per_agent_fallback_and_routing_optional_and_legacy_safe() {
        // Load fixture (no fallback_chain / routing) — legacy safe.
        let yaml = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            p.fallback_chain.is_empty(),
            "legacy profile must have empty fallback_chain"
        );
        assert!(
            p.routing.is_none(),
            "legacy profile must have no routing override"
        );

        // Round-trip with fallback_chain and routing.
        let mut p = p.clone();
        p.fallback_chain = vec!["claude_opus".into(), "claude_sonnet".into()];
        p.routing = Some(crate::config::RoutingConfig {
            enabled: true,
            ..Default::default()
        });
        let s = serde_yaml_ng::to_string(&p).unwrap();
        assert!(
            s.contains("fallback_chain:"),
            "yaml must contain fallback_chain"
        );
        assert!(s.contains("routing:"), "yaml must contain routing");
        let p2: AgentProfile = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(
            p2.fallback_chain,
            vec!["claude_opus", "claude_sonnet"],
            "fallback_chain must round-trip"
        );
        assert!(
            p2.routing.as_ref().unwrap().enabled,
            "routing.enabled must round-trip"
        );
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

#[cfg(test)]
mod mcp_pin_tests {
    use super::*;

    /// Pre-M9 profiles must continue to deserialize with the new
    /// optional fields absent. Round-trip: serialize back out and
    /// confirm the optional fields don't leak into the YAML.
    #[test]
    fn pre_m9_entry_roundtrips_without_pin_fields() {
        let yaml = r#"
name: weather
command: /opt/mcp/weather
args: ["--port", "0"]
"#;
        let entry: McpServerEntry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(entry.name, "weather");
        assert_eq!(entry.binary_sha256, None);
        assert_eq!(entry.description_hash, None);
        assert_eq!(entry.publisher, None);
        assert_eq!(entry.installed_at, None);

        // skip_serializing_if = "Option::is_none" must keep the YAML
        // free of empty pin fields when the entry is pre-M9.
        let out = serde_yaml_ng::to_string(&entry).unwrap();
        assert!(!out.contains("binary_sha256"), "got {out}");
        assert!(!out.contains("description_hash"), "got {out}");
        assert!(!out.contains("publisher"), "got {out}");
        assert!(!out.contains("installed_at"), "got {out}");
    }

    /// Full M9 entry with all fields set round-trips losslessly.
    #[test]
    fn full_m9_entry_roundtrips_all_fields() {
        let yaml = r#"
name: weather
command: /opt/mcp/weather
args: []
binary_sha256: "3f4abca8b0e6e2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b81c"
description_hash: "9a01b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9c7e2"
publisher:
  name: "@anthropic-mcp/weather"
  homepage: "https://github.com/anthropic-mcp/weather"
  registry_id: "@anthropic-mcp/weather@1.2.3"
installed_at: "2026-05-06T08:00:00Z"
"#;
        let entry: McpServerEntry = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            entry
                .binary_sha256
                .as_deref()
                .unwrap()
                .starts_with("3f4abca8")
        );
        assert!(
            entry
                .description_hash
                .as_deref()
                .unwrap()
                .starts_with("9a01b2c3")
        );
        let pub_info = entry.publisher.clone().unwrap();
        assert_eq!(pub_info.name, "@anthropic-mcp/weather");
        assert_eq!(
            pub_info.homepage.as_deref(),
            Some("https://github.com/anthropic-mcp/weather"),
        );
        assert_eq!(
            pub_info.registry_id.as_deref(),
            Some("@anthropic-mcp/weather@1.2.3"),
        );
        let installed = entry.installed_at.unwrap();
        assert_eq!(installed.to_rfc3339(), "2026-05-06T08:00:00+00:00");
    }

    /// Partial — only the binary hash is set (e.g. probe failed but
    /// install proceeded). The supervisor still needs to be able to
    /// deserialize this without panicking.
    #[test]
    fn partial_pin_only_binary_sha_roundtrips() {
        let yaml = r#"
name: weather
command: /opt/mcp/weather
args: []
binary_sha256: "deadbeef00112233445566778899aabbccddeeff00112233445566778899aabb"
"#;
        let entry: McpServerEntry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(
            entry.binary_sha256.as_deref(),
            Some("deadbeef00112233445566778899aabbccddeeff00112233445566778899aabb"),
        );
        assert_eq!(entry.description_hash, None);
        assert_eq!(entry.publisher, None);
    }

    /// Publisher with only the required `name` field — homepage and
    /// registry_id are optional.
    #[test]
    fn publisher_minimal_just_name() {
        let yaml = r#"
name: weather
command: /opt/mcp/weather
args: []
publisher:
  name: "alice"
"#;
        let entry: McpServerEntry = serde_yaml_ng::from_str(yaml).unwrap();
        let p = entry.publisher.as_ref().unwrap();
        assert_eq!(p.name, "alice");
        assert_eq!(p.homepage, None);
        assert_eq!(p.registry_id, None);

        // skip_serializing_if must omit the optional sub-fields too.
        let out = serde_yaml_ng::to_string(&entry).unwrap();
        assert!(!out.contains("homepage:"), "got {out}");
        assert!(!out.contains("registry_id:"), "got {out}");
    }
}

#[cfg(test)]
mod voice_tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn voice_config_round_trips() {
        // Base: use the canonical minimal fixture and append a voice: block.
        let base = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let yaml = format!("{base}voice:\n  enabled: true\n  voice_id: af_bella\n");

        let profile: AgentProfile = serde_yaml_ng::from_str(&yaml).expect("parse with voice");
        assert!(profile.voice.enabled);
        assert_eq!(profile.voice.voice_id, VoiceId::AfBella);

        // Legacy profiles (no voice: block) must still load.
        let legacy: AgentProfile = serde_yaml_ng::from_str(base).expect("parse without voice");
        assert!(!legacy.voice.enabled);
        assert_eq!(legacy.voice.voice_id, VoiceId::AfHeart);
    }

    #[test]
    fn voice_id_from_str_roundtrips() {
        let cases = [
            ("af_heart", VoiceId::AfHeart),
            ("af_bella", VoiceId::AfBella),
            ("af_nicole", VoiceId::AfNicole),
            ("am_adam", VoiceId::AmAdam),
            ("am_michael", VoiceId::AmMichael),
        ];
        for (s, expected) in cases {
            assert_eq!(VoiceId::from_str(s).unwrap(), expected);
            assert_eq!(expected.as_str(), s);
        }
    }

    #[test]
    fn voice_id_from_str_rejects_unknown() {
        assert!(VoiceId::from_str("bogus").is_err());
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
mod appearance_tests {
    use super::*;

    #[test]
    fn appearance_default_style_preset_is_default_blob() {
        assert_eq!(AgentAppearance::default().style_preset, "default-blob");
    }

    #[test]
    fn appearance_default_behavior_is_normal() {
        assert_eq!(
            AgentAppearance::default().behavior_preset,
            BehaviorPreset::Normal
        );
    }

    #[test]
    fn appearance_default_render_status_is_pending() {
        assert_eq!(
            AgentAppearance::default().render_status,
            RenderStatus::Pending
        );
    }

    #[test]
    fn render_status_serde_round_trip() {
        let cases = [
            RenderStatus::Pending,
            RenderStatus::Rendering { done: 3, total: 12 },
            RenderStatus::Ready,
            RenderStatus::Failed {
                reason: "out of quota".into(),
            },
        ];
        for status in cases {
            let yaml = serde_yaml_ng::to_string(&status).expect("serialize");
            let back: RenderStatus = serde_yaml_ng::from_str(&yaml).expect("deserialize");
            assert_eq!(status, back);
        }
    }

    #[test]
    fn agent_profile_with_appearance_round_trips() {
        let base = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let yaml = format!(
            "{base}appearance:\n  style_preset: chiikawa\n  render_status:\n    status: ready\n"
        );
        let profile: AgentProfile = serde_yaml_ng::from_str(&yaml).expect("parse with appearance");
        assert_eq!(profile.appearance.style_preset, "chiikawa");
        assert_eq!(profile.appearance.render_status, RenderStatus::Ready);

        let out = serde_yaml_ng::to_string(&profile).expect("serialize");
        let back: AgentProfile = serde_yaml_ng::from_str(&out).expect("re-parse");
        assert_eq!(profile.appearance, back.appearance);
    }

    #[test]
    fn legacy_profile_without_appearance_uses_default() {
        let yaml = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let profile: AgentProfile = serde_yaml_ng::from_str(yaml).expect("parse legacy");
        assert_eq!(profile.appearance.style_preset, "default-blob");
        assert_eq!(profile.appearance.behavior_preset, BehaviorPreset::Normal);
        assert_eq!(profile.appearance.render_status, RenderStatus::Pending);
    }

    #[test]
    fn legacy_profile_without_file_actions_or_action_pipeline_loads() {
        let yaml = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(p.file_actions.is_empty());
        assert_eq!(p.action_pipeline.deletion.cancel_window_minutes, 10);
        assert_eq!(p.action_pipeline.queue.max_concurrent, 3);
    }
}

#[cfg(test)]
mod federation_tests {
    use super::*;

    #[test]
    fn test_pattern_filter_default() {
        let f = PatternFilter::default();
        assert_eq!(f.max_count, 200);
        assert_eq!(f.importance_min, 0.0);
        assert!(f.tier.is_empty());
    }

    #[test]
    fn test_federation_config_roundtrip() {
        let cfg = FederationConfig {
            filter: PatternFilter {
                tier: vec!["core".into()],
                max_count: 50,
                ..Default::default()
            },
            snapshot_ref: Some(SnapshotRef {
                knowledge_commit: "abc123def456".into(),
                taken_at: "2026-05-19T00:00:00Z".into(),
                filter: PatternFilter::default(),
            }),
            evidence_flush_interval_minutes: 15,
        };
        let yaml = serde_yaml_ng::to_string(&cfg).unwrap();
        let back: FederationConfig = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn test_agent_profile_federation_defaults() {
        // AgentProfile without a federation block deserializes with FederationConfig::default().
        // Use the minimal YAML that passes validation — just the required fields.
        // (We check only that the field has its zero value, not full profile parse.)
        let cfg = FederationConfig::default();
        assert_eq!(cfg.evidence_flush_interval_minutes, 0);
        assert!(cfg.snapshot_ref.is_none());
    }
}

#[cfg(test)]
mod skill_card_tests {
    use super::*;

    #[test]
    fn installed_skills_default_to_empty_when_absent() {
        let yaml = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(p.installed_skills.is_empty());
    }

    #[test]
    fn installed_skills_roundtrip_preserves_entries() {
        let base = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let yaml = format!(
            "{base}installed_skills:\n  - name: s1\n    version: 1.0.0\n    publisher: human:d\n    description: desc\n    category: workflow\n    tags: [web]\n    triggers:\n      - type: command\n        pattern: /find\n    abstract: does things\n    transfer_chain:\n      - agent://alice\n"
        );
        let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(p.installed_skills.len(), 1);
        assert_eq!(p.installed_skills[0].name, "s1");
        assert_eq!(p.installed_skills[0].abstract_text, "does things");
        assert_eq!(p.installed_skills[0].transfer_chain, vec!["agent://alice"]);

        let out = serde_yaml_ng::to_string(&p).unwrap();
        assert!(out.contains("abstract: does things"));
        assert!(out.contains("pattern: /find"));

        let back: AgentProfile = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(p.installed_skills, back.installed_skills);
    }

    #[test]
    fn installed_skills_minimal_entry_serializes_compactly() {
        // A name-only entry must NOT emit empty string fields.
        let entry = SkillCardEntry {
            name: "minimal".into(),
            ..Default::default()
        };
        let yaml = serde_yaml_ng::to_string(&entry).unwrap();
        assert!(yaml.contains("name: minimal"));
        assert!(
            !yaml.contains("version:"),
            "empty version must be skipped: {yaml}"
        );
        assert!(
            !yaml.contains("publisher:"),
            "empty publisher must be skipped: {yaml}"
        );
        assert!(
            !yaml.contains("abstract:"),
            "empty abstract must be skipped: {yaml}"
        );
    }
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
        let mut p = AgentProfile::default_for_tests();
        p.addons.push(AddonRef {
            id: "grp".into(),
            source: "claude-local:grp@1.0.0".into(),
            enabled: false,
            skills: vec!["g_skill".into()],
            mcp: vec!["g_mcp".into()],
            commands: vec!["g_cmd".into()],
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

#[cfg(test)]
mod remote_mcp_tests {
    use super::*;

    #[test]
    fn mcp_entry_roundtrips_remote_bearer() {
        let e = McpServerEntry {
            name: "gh".into(),
            command: String::new(),
            url: Some("https://api.example.com/mcp".into()),
            auth: Some(McpAuth::Bearer {
                token: crate::secret::SecretRef::Env("GH_TOKEN".into()),
            }),
            ..Default::default()
        };
        let y = serde_yaml_ng::to_string(&e).unwrap();
        let back: McpServerEntry = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(back.url.as_deref(), Some("https://api.example.com/mcp"));
        assert!(matches!(
            back.auth,
            Some(McpAuth::Bearer { ref token }) if *token == crate::secret::SecretRef::Env("GH_TOKEN".into())
        ));
        // A legacy stdio entry (no url/auth) still parses.
        let legacy: McpServerEntry =
            serde_yaml_ng::from_str("name: fs\ncommand: npx\nargs: [\"-y\",\"fs\"]\n").unwrap();
        assert!(legacy.url.is_none());
        assert!(legacy.auth.is_none());
    }
}

#[cfg(test)]
mod requires_programs_tests {
    #[test]
    fn mcp_entry_parses_requires_programs_and_defaults_empty() {
        let with = r#"
name: research-gateway
command: mur-research-gateway
requires_programs:
  - name: lightpanda
    detect: { file: "~/.mur/aura/lightpanda" }
    reason: "render tier"
    registry: lightpanda
"#;
        let e: crate::agent::McpServerEntry = serde_yaml::from_str(with).unwrap();
        assert_eq!(e.requires_programs.len(), 1);
        assert_eq!(e.requires_programs[0].name, "lightpanda");

        // Absent block → empty (back-compat).
        let without = "name: x\ncommand: y\n";
        let e2: crate::agent::McpServerEntry = serde_yaml::from_str(without).unwrap();
        assert!(e2.requires_programs.is_empty());
    }
}
