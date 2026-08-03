use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_LOCAL_LLM_MODEL: &str = "qwen3.5:4b";

/// Default model id seeded for the built-in "Mur" agent and used to name the
/// bundled MLX weights. This is the DEFAULT VALUE only — it is written into the
/// seed agent's profile and can be changed by the user afterwards; it is not a
/// behavioural constant baked into logic.
pub const DEFAULT_BUNDLED_MODEL_ID: &str = "Qwen3.5-2B-MLX-4bit";

pub const DEFAULT_MAX_RETRIES: u32 = 1;
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 500;
pub const DEFAULT_COOLDOWN_SECS: u64 = 60;
pub const DEFAULT_ROUTING_THRESHOLD: u32 = 2000;
pub const DEFAULT_SMART_MAX_ESCALATIONS: u32 = 1;

fn default_max_retries() -> u32 {
    DEFAULT_MAX_RETRIES
}
fn default_backoff_base_ms() -> u64 {
    DEFAULT_BACKOFF_BASE_MS
}
fn default_cooldown_secs() -> u64 {
    DEFAULT_COOLDOWN_SECS
}
fn default_smart_max_escalations() -> u32 {
    DEFAULT_SMART_MAX_ESCALATIONS
}

/// Smart background routing: auto-pick a cheap model for low-stakes/background
/// requests instead of always dialing the agent's primary model_ref. Defaults
/// ON with `cheap: None` (auto-pick the cheapest chat-capable registry entry
/// via `mur_common::model::pick_cheap_model`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmartConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,
    #[serde(default = "default_smart_max_escalations")]
    pub max_escalations: u32,
}

impl Default for SmartConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cheap: None,
            max_escalations: DEFAULT_SMART_MAX_ESCALATIONS,
        }
    }
}

/// Config-layered model selection + failure fallback. See
/// docs/superpowers/specs/2026-07-12-intelligent-model-switch-design.md.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelSwitchConfig {
    /// Global default model_ref when an agent has no `model_ref`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Global fallback chain (ordered model_refs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_chain: Vec<String>,
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub smart: SmartConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_backoff_base_ms")]
    pub backoff_base_ms: u64,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            backoff_base_ms: DEFAULT_BACKOFF_BASE_MS,
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RoutingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart: Option<SmartConfig>,
}

/// Global MUR configuration (~/.mur/config.yaml)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub embedding: EmbeddingConfig,

    #[serde(default)]
    pub llm: LlmConfig,

    #[serde(default)]
    pub models: ModelSwitchConfig,

    #[serde(default)]
    pub retrieval: RetrievalConfig,

    #[serde(default)]
    pub paths: PathConfig,

    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub community: CommunityConfig,

    #[serde(default)]
    pub conversations: ConversationsConfig,

    #[serde(default)]
    pub sync: SyncConfig,

    // --- P1.1 additions ---
    #[serde(default)]
    pub storage: StorageConfig,

    #[serde(default)]
    pub sources_global: SourcesGlobalConfig,

    // --- E3 additions ---
    #[serde(default)]
    pub sleep_cycle: SleepCycleConfig,

    // --- M2 additions ---
    #[serde(default)]
    pub skills: SkillsConfig,

    // --- M6c additions ---
    #[serde(default)]
    pub skill_llm: SkillLlmConfig,

    // --- M7a additions ---
    #[serde(default)]
    pub cross_agent: CrossAgentConfig,

    // --- nudge additions ---
    #[serde(default)]
    pub nudge: NudgeConfig,

    // --- mobile P4 additions ---
    #[serde(default)]
    pub mobile_relay: MobileRelayConfig,

    // --- Ambient capture & harvest (2026-06-11 spec) ---
    #[serde(default)]
    pub session: SessionCfg,

    #[serde(default)]
    pub harvest: HarvestCfg,

    // --- OAuth bridge (cc-proxy) routing for subscription tokens ---
    #[serde(default)]
    pub cc_proxy: CcProxyConfig,

    // --- Agent CLI TUI ---
    #[serde(default)]
    pub cli: CliConfig,

    // --- parallel_jobs MCP tool ---
    #[serde(default)]
    pub parallel_jobs: ParallelJobsConfig,

    // --- fleet_run runtime built-in tool ---
    #[serde(default)]
    pub fleet_run: FleetRunConfig,

    // --- `mur open` display policy ---
    #[serde(default)]
    pub open_items: OpenItemsConfig,

    // --- Hub Fleet Manager redesign ---
    #[serde(default)]
    pub fleet: FleetConfig,
}

/// Authorization gate for the `parallel_jobs` MCP tool. Stored under `parallel_jobs:`
/// in `~/.mur/config.yaml`. Deny-by-default: an empty `targets` list means the
/// tool cannot delegate to ANY agent (inert until the user opts specific
/// agents in). This is a deterministic, out-of-model gate that a
/// prompt-injected concierge cannot widen (OWASP Agentic ASI02/03/04).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ParallelJobsConfig {
    /// Canonical agent names the `parallel_jobs` tool is allowed to delegate to.
    /// Empty = deny all.
    #[serde(default)]
    pub targets: Vec<String>,
}

/// Authorization gate for the runtime's built-in `fleet_run` tool. Stored under
/// `fleet_run:` in `~/.mur/config.yaml`. Deny-by-default on BOTH axes: an agent
/// not named in `agents` never even sees the tool, and a fleet not named in
/// `fleets` cannot be run. Lives in the global config (not the agent profile)
/// because the profile is writable by the concierge itself — this gate must be
/// out of reach of a prompt-injected agent (same rationale as
/// [`ParallelJobsConfig`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FleetRunConfig {
    /// Canonical agent names allowed to call `fleet_run`. Empty = deny all.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Fleet names those agents may run. Empty = deny all.
    #[serde(default)]
    pub fleets: Vec<String>,
}

/// Display policy for `mur open`.
///
/// Lives in `config.yaml` rather than in `open-items.jsonl` because that log
/// is append-only and agent-writable via the `open_item` tool. A user's
/// decision to stop looking at a source must not be overturnable by an agent
/// appending a record. Same reasoning as `fleet_run.agents`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OpenItemsConfig {
    /// Exact `origin` strings to collapse out of `mur open`. Exact match
    /// only — `fleet` never matches `fleet:acme`.
    #[serde(default)]
    pub muted: Vec<String>,
}

/// Daemon-wide gate for unattended fleet auto-run (`mur-daemon`'s `fleet_tick`).
/// Stored under `fleet:` in `~/.mur/config.yaml`. Either this flag OR the
/// `MUR_FLEET_AUTORUN` env var satisfies the gate — both are equally explicit,
/// off-by-default opt-ins; the env var remains for ops/CI use, this flag is
/// what the Hub's Settings toggle controls. Per-fleet `budget_usd > 0` and the
/// `.stopped` kill-switch are unaffected — see `mur-daemon/src/fleet_tick.rs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FleetConfig {
    /// Allow fleets with a trigger + budget configured to auto-run unattended.
    #[serde(default)]
    pub autorun: bool,
}

#[cfg(test)]
mod fleet_config_tests {
    use super::*;

    #[test]
    fn fleet_config_defaults_off_and_roundtrips() {
        assert!(!FleetConfig::default().autorun);

        let cfg: Config = serde_yaml_ng::from_str("fleet:\n  autorun: true\n").unwrap();
        assert!(cfg.fleet.autorun);

        // `fleet:` key entirely absent → defaults to off
        let cfg2: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert!(!cfg2.fleet.autorun);
    }

    #[test]
    fn fleet_run_config_defaults_deny_all_and_roundtrips() {
        // Absent section → both allowlists empty → deny all.
        let cfg: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert!(cfg.fleet_run.agents.is_empty());
        assert!(cfg.fleet_run.fleets.is_empty());

        let cfg2: Config =
            serde_yaml_ng::from_str("fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n")
                .unwrap();
        assert_eq!(cfg2.fleet_run.agents, vec!["mur"]);
        assert_eq!(cfg2.fleet_run.fleets, vec!["deep-research"]);
    }
}

/// Routing for Anthropic subscription-OAuth (`sk-ant-oat*`) tokens through a
/// local bridge — cc-proxy — that swaps `x-api-key` for the Bearer +
/// claude-code betas disguise the upstream requires.
///
/// The Hub injects `ANTHROPIC_BASE_URL` pointing at [`url`](Self::url) when it
/// spawns an agent runtime, but only when [`enabled`](Self::enabled) is set and
/// the bridge is actually listening; otherwise it leaves the runtime on the
/// direct `api.anthropic.com` path (where an oat token would 401). A runtime
/// launched with `ANTHROPIC_BASE_URL` already in its environment is never
/// overridden.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CcProxyConfig {
    /// Bridge base URL. Defaults to cc-proxy's default bind.
    #[serde(default = "default_cc_proxy_url")]
    pub url: String,

    /// Master switch. When false the Hub never routes runtimes through the
    /// bridge, regardless of reachability.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_cc_proxy_url() -> String {
    "http://127.0.0.1:8088".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for CcProxyConfig {
    fn default() -> Self {
        Self {
            url: default_cc_proxy_url(),
            enabled: true,
        }
    }
}

/// Configuration for the agent CLI TUI.
/// Stored in ~/.mur/config.yaml under the `cli:` key.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliConfig {
    /// Default visual skin for `mur agent cli`. Overridable with --skin.
    /// Valid values: "dark" (default), "light", "mur".
    pub skin: Option<String>,
}

/// Configuration for the mobile relay (P4).
/// Stored in ~/.mur/config.yaml under the `mobile_relay:` key.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MobileRelayConfig {
    /// Base URL of the mur-server relay, e.g. "wss://relay.mur.run".
    /// Leave blank to disable relay forwarding on the Mac daemon side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,

    /// API key or JWT used by the Mac daemon to authenticate with the relay.
    /// The value is typically a `mur_...` API key from app.mur.run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl Config {
    /// Read from disk, falling back to defaults.
    pub fn load_or_default(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_yaml_ng::from_str(&s).ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncConfig {
    /// Sync method: "cloud", "git", or "local"
    #[serde(default = "default_sync_method")]
    pub method: String,

    /// Git remote URL for git sync
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remote: Option<String>,

    /// Auto-sync on context pull / session stop
    #[serde(default)]
    pub auto: bool,

    /// Default team ID for cloud sync (set on first successful sync)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
}

fn default_sync_method() -> String {
    "local".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Server URL (default: https://mur-server.fly.dev)
    #[serde(default = "default_server_url")]
    pub url: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            url: default_server_url(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommunityConfig {
    /// Whether community pattern sharing is enabled
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// "ollama", "openai", "gemini", or "anthropic"
    #[serde(default = "default_embedding_provider")]
    pub provider: String,

    /// Model name (e.g. "nomic-embed-text", "text-embedding-3-small")
    #[serde(default = "default_embedding_model")]
    pub model: String,

    /// Vector dimensions (fixed after first index build)
    #[serde(default = "default_dimensions")]
    pub dimensions: usize,

    /// Ollama endpoint
    #[serde(default = "default_ollama_endpoint")]
    pub ollama_endpoint: String,

    /// API key env var name (e.g. "OPENAI_API_KEY")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,

    /// SecretRef string for the API key (e.g. "keychain:mur/anthropic",
    /// "env:ANTHROPIC_API_KEY"). Takes precedence over `api_key_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,

    /// Custom OpenAI-compatible API URL (e.g. for OpenRouter)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_url: Option<String>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: default_embedding_provider(),
            model: default_embedding_model(),
            dimensions: default_dimensions(),
            ollama_endpoint: default_ollama_endpoint(),
            api_key_env: None,
            api_key_ref: None,
            openai_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// "anthropic", "openai", "gemini", or "ollama"
    #[serde(default = "default_llm_provider")]
    pub provider: String,

    #[serde(default = "default_llm_model")]
    pub model: String,

    /// API key env var name (e.g. "ANTHROPIC_API_KEY")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,

    /// SecretRef string for the API key (e.g. "keychain:mur/anthropic",
    /// "env:ANTHROPIC_API_KEY"). Takes precedence over `api_key_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,

    /// Custom OpenAI-compatible API URL (e.g. for OpenRouter)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_url: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: default_llm_provider(),
            model: default_llm_model(),
            api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
            api_key_ref: None,
            openai_url: None,
        }
    }
}

impl LlmConfig {
    /// Convert legacy LlmConfig (used by extract_llm, learn, capture/starter)
    /// into a BackendConfig that the new ChatBackend factory consumes.
    /// Mapping:
    /// - `provider` 1:1, except: unknown providers WITH openai_url become "openai"
    ///   (preserves the historical LlmConfig::llm_complete fall-through for
    ///   OpenAI-compatible passthrough proxies).
    /// - `model` 1:1.
    /// - `api_key_env` 1:1 (factory's resolve_api_key falls back to
    ///   default_key_env(provider) when None — preserves LlmConfig behavior).
    /// - `openai_url` → `endpoint` (semantic rename; same string semantics).
    /// - `timeout_secs` always None (factory defaults to 120s — matches
    ///   the historical 60s reqwest default behavior closely enough).
    pub fn to_backend_config(&self) -> BackendConfig {
        let provider = match self.provider.as_str() {
            "anthropic" | "openai" | "openrouter" | "gemini" | "ollama" => self.provider.clone(),
            _ if self.openai_url.is_some() => "openai".into(),
            other => other.into(), // factory will reject with "unsupported provider"
        };
        BackendConfig {
            provider,
            model: self.model.clone(),
            endpoint: self.openai_url.clone(),
            api_key_env: self.api_key_env.clone(),
            api_key_ref: self.api_key_ref.clone(),
            timeout_secs: None,
        }
    }
}

/// Backend selection for a single chat-completion call site.
///
/// Per spec §6 of cloud-LLM-backend design. Used by `CompactConfig`
/// (per-stage) and `AskConfig` (per-stage) to override the legacy
/// Ollama-only path. None of the `Option` fields are required;
/// resolution falls back to provider defaults
/// (ollama: http://localhost:11434, anthropic: https://api.anthropic.com).
///
/// Stays in mur-common (not mur-core) because it is pure data and
/// will be reused by mur-agent-runtime in a future phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BackendConfig {
    /// "ollama" | "anthropic". Defaults to "ollama" for backward compat.
    pub provider: String,
    /// Model name as the provider sees it ("claude-haiku-4-5", "qwen3:4b", …).
    pub model: String,
    /// Provider endpoint. None = provider default
    /// (ollama: http://localhost:11434, anthropic: https://api.anthropic.com).
    pub endpoint: Option<String>,
    /// Env var holding the API key. None = no auth (ollama).
    pub api_key_env: Option<String>,
    /// SecretRef string for the API key. Takes precedence over `api_key_env`.
    pub api_key_ref: Option<String>,
    /// Per-call timeout in seconds. None = 120s.
    pub timeout_secs: Option<u64>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            provider: "ollama".into(),
            model: DEFAULT_LOCAL_LLM_MODEL.into(),
            endpoint: None,
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfig {
    /// Max patterns to inject per query
    #[serde(default = "default_max_patterns")]
    pub max_patterns: usize,

    /// Max tokens for injected content
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,

    /// Minimum score threshold
    #[serde(default = "default_min_score")]
    pub min_score: f64,

    /// MMR diversity threshold (cosine > this = too similar)
    #[serde(default = "default_mmr_threshold")]
    pub mmr_threshold: f64,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            max_patterns: default_max_patterns(),
            max_tokens: default_max_tokens(),
            min_score: default_min_score(),
            mmr_threshold: default_mmr_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathConfig {
    /// Root MUR directory (default: ~/.mur)
    #[serde(default = "default_mur_dir")]
    pub mur_dir: PathBuf,
}

impl Default for PathConfig {
    fn default() -> Self {
        Self {
            mur_dir: default_mur_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Vector backend identifier: "lancedb" (default) or "qdrant".
    #[serde(default = "default_vector_backend")]
    pub vector_backend: String,

    /// Qdrant connection URL (only used when vector_backend = "qdrant").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qdrant_url: Option<String>,

    /// Keyring account name holding the Qdrant API key, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qdrant_api_key_ref: Option<String>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            vector_backend: default_vector_backend(),
            qdrant_url: None,
            qdrant_api_key_ref: None,
        }
    }
}

fn default_vector_backend() -> String {
    "lancedb".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcesGlobalConfig {
    /// Polling interval for cloud sources (seconds).
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,

    /// Safety cap: do not sync more than this many chunks per run.
    #[serde(default = "default_max_chunks_per_sync")]
    pub max_chunks_per_sync: usize,

    /// Upper bound on parallel source sync tasks.
    #[serde(default = "default_max_parallel_sources")]
    pub max_parallel_sources: usize,

    /// Weight applied to new sources unless overridden.
    #[serde(default = "default_source_weight")]
    pub default_weight: f32,

    /// Embedding request batch size.
    #[serde(default = "default_embedding_batch_size")]
    pub embedding_batch_size: usize,
}

impl Default for SourcesGlobalConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval_secs(),
            max_chunks_per_sync: default_max_chunks_per_sync(),
            max_parallel_sources: default_max_parallel_sources(),
            default_weight: default_source_weight(),
            embedding_batch_size: default_embedding_batch_size(),
        }
    }
}

fn default_poll_interval_secs() -> u64 {
    600
}
fn default_max_chunks_per_sync() -> usize {
    10_000
}
fn default_max_parallel_sources() -> usize {
    3
}
fn default_source_weight() -> f32 {
    1.0
}
fn default_embedding_batch_size() -> usize {
    32
}

fn default_embedding_provider() -> String {
    "ollama".to_string()
}
fn default_embedding_model() -> String {
    "qwen3-embedding:0.6b".to_string()
}
fn default_dimensions() -> usize {
    1024
}
fn default_ollama_endpoint() -> String {
    "http://localhost:11434".to_string()
}
fn default_llm_provider() -> String {
    "anthropic".to_string()
}
fn default_llm_model() -> String {
    "claude-opus-5".to_string()
}
fn default_max_patterns() -> usize {
    5
}
fn default_max_tokens() -> usize {
    2000
}
fn default_min_score() -> f64 {
    0.35
}
fn default_mmr_threshold() -> f64 {
    0.85
}
fn default_mur_dir() -> PathBuf {
    // Use HOME env var directly to avoid the `dirs` dependency in mur-common.
    // Callers in mur-core that need the real home dir should use `dirs` there.
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    home.join(".mur")
}
fn default_server_url() -> String {
    "https://mur-server.fly.dev".to_string()
}

// ── Ask config (Phase 2B, Task 18) ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskConfig {
    #[serde(default = "ask_default_model")]
    pub model: String,
    #[serde(default = "compact_default_ollama_endpoint")]
    pub ollama_endpoint: String,
    #[serde(default = "ask_default_k_summary")]
    pub k_summary: u32,
    #[serde(default = "ask_default_k_raw")]
    pub k_raw: u32,
    #[serde(default = "ask_default_esc")]
    pub escalation_threshold: f64,
    #[serde(default = "ask_default_mmr")]
    pub mmr_threshold: f64,
    #[serde(default = "ask_default_max_ctx")]
    pub max_context_tokens: u32,
    #[serde(default = "ask_default_resp_tok")]
    pub response_tokens: u32,
    #[serde(default = "ask_default_timeout")]
    pub timeout_secs: u32,
    #[serde(default = "ask_default_min_score")]
    pub min_score: f64,
    #[serde(default = "ask_default_continue_history_turns")]
    pub continue_history_turns: u32,
    /// Separate, shorter timeout for the rewriter LLM call (Phase 3.3).
    /// Rewriter output is small (~80 tokens) and falling back to the raw
    /// question on failure is non-fatal, so we don't want to burn the full
    /// `timeout_secs` budget waiting on a slow/unreachable Ollama before
    /// the user sees any response.
    #[serde(default = "ask_default_rewriter_timeout")]
    pub rewriter_timeout_secs: u32,
    #[serde(default = "ask_default_compress_hits_enabled")]
    pub compress_hits_enabled: bool,
    #[serde(default = "ask_default_summarize_hits_enabled")]
    pub summarize_hits_enabled: bool,
    #[serde(default)]
    pub summarize_model: Option<String>,
    /// Per-stage backend override for the answer-generation model.
    /// None = synthesize from legacy `model` + `ollama_endpoint`.
    #[serde(default)]
    pub backend: Option<BackendConfig>,
    /// Per-stage backend override for the query rewriter.
    /// None = synthesize an Ollama BackendConfig over the legacy `model` +
    /// `ollama_endpoint` with `rewriter_timeout_secs` baked in.
    #[serde(default)]
    pub rewriter_backend: Option<BackendConfig>,
}

impl AskConfig {
    /// Returns the effective backend for the answer-generation model.
    /// Per-stage `backend` override wins; otherwise synthesize from legacy
    /// fields (`model`, `ollama_endpoint`) into an Ollama BackendConfig.
    ///
    /// `timeout_secs` is baked from `self.timeout_secs` so the answer call
    /// inherits the user's per-call budget (rather than factory's 120s
    /// default). When the user supplied an explicit `backend` override
    /// with its own `timeout_secs`, that wins — we only synthesize when
    /// `self.backend` is None.
    pub fn synthesize_backend(&self) -> BackendConfig {
        self.backend.clone().unwrap_or_else(|| BackendConfig {
            provider: "ollama".into(),
            model: self.model.clone(),
            endpoint: Some(self.ollama_endpoint.clone()),
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: Some(self.timeout_secs as u64),
        })
    }

    /// Returns the effective backend for the query rewriter.
    ///
    /// When `self.rewriter_backend` is None, this synthesizes its OWN
    /// Ollama BackendConfig with `self.rewriter_timeout_secs` baked in —
    /// it does NOT fall through to `synthesize_backend()`. The rewriter
    /// has a much tighter latency budget than the answer call (rewriter
    /// output is small and falling back to the raw question on timeout
    /// is non-fatal), so we don't want a slow Ollama burning the full
    /// `timeout_secs` budget before the user sees any response.
    pub fn synthesize_rewriter_backend(&self) -> BackendConfig {
        self.rewriter_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                provider: "ollama".into(),
                model: self.model.clone(),
                endpoint: Some(self.ollama_endpoint.clone()),
                api_key_env: None,
                api_key_ref: None,
                timeout_secs: Some(self.rewriter_timeout_secs as u64),
            })
    }

    /// Effective backend for answer generation. An explicit per-stage
    /// override wins; otherwise the stage inherits the smart slot
    /// (`config.llm`) with this stage's own timeout baked in, so a slow
    /// backend cannot silently fall back to the factory's 120s default.
    pub fn effective_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.backend.clone().unwrap_or_else(|| BackendConfig {
            timeout_secs: Some(self.timeout_secs as u64),
            ..llm.to_backend_config()
        })
    }

    /// Effective backend for the query rewriter. Deliberately does NOT fall
    /// through to `effective_backend`: the rewriter's output is small and
    /// falling back to the raw question on timeout is non-fatal, so it keeps
    /// the much tighter `rewriter_timeout_secs` budget.
    pub fn effective_rewriter_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.rewriter_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(self.rewriter_timeout_secs as u64),
                ..llm.to_backend_config()
            })
    }
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            model: ask_default_model(),
            ollama_endpoint: compact_default_ollama_endpoint(),
            k_summary: ask_default_k_summary(),
            k_raw: ask_default_k_raw(),
            escalation_threshold: ask_default_esc(),
            mmr_threshold: ask_default_mmr(),
            max_context_tokens: ask_default_max_ctx(),
            response_tokens: ask_default_resp_tok(),
            timeout_secs: ask_default_timeout(),
            min_score: ask_default_min_score(),
            continue_history_turns: ask_default_continue_history_turns(),
            rewriter_timeout_secs: ask_default_rewriter_timeout(),
            compress_hits_enabled: ask_default_compress_hits_enabled(),
            summarize_hits_enabled: ask_default_summarize_hits_enabled(),
            summarize_model: None,
            backend: None,
            rewriter_backend: None,
        }
    }
}

fn ask_default_model() -> String {
    DEFAULT_LOCAL_LLM_MODEL.into()
}
fn ask_default_k_summary() -> u32 {
    5
}
fn ask_default_k_raw() -> u32 {
    10
}
fn ask_default_esc() -> f64 {
    0.5
}
fn ask_default_mmr() -> f64 {
    0.88
}
fn ask_default_max_ctx() -> u32 {
    6000
}
fn ask_default_resp_tok() -> u32 {
    1024
}
fn ask_default_timeout() -> u32 {
    120
}
fn ask_default_min_score() -> f64 {
    0.35
}
fn ask_default_rewriter_timeout() -> u32 {
    8
}
fn ask_default_continue_history_turns() -> u32 {
    3
}
fn ask_default_compress_hits_enabled() -> bool {
    true
}
fn ask_default_summarize_hits_enabled() -> bool {
    true
}

// ── Conversations archive config (Task 23) ────────────────────────────────────

/// Phase 1 conversations archive config (Task 23).
///
/// Hard defaults: off-by-default (`enabled: false`), 30-day retention,
/// 5-minute poll interval, all sources enabled, Mem0-style REJECT filters on,
/// dedup threshold 0.85. Every sub-field is serde-default so a config.yaml
/// without a `conversations:` section still parses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "conv_default_retention_days")]
    pub retention_days: u32,
    #[serde(default = "conv_default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub sources: ConversationsSources,
    #[serde(default)]
    pub filter: ConversationsFilter,
    #[serde(default)]
    pub compact: CompactConfig,
    #[serde(default)]
    pub ask: AskConfig,
    #[serde(default)]
    pub rollup: RollupConfig,
}

impl Default for ConversationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            retention_days: conv_default_retention_days(),
            poll_interval_secs: conv_default_poll_interval(),
            sources: ConversationsSources::default(),
            filter: ConversationsFilter::default(),
            compact: CompactConfig::default(),
            ask: AskConfig::default(),
            rollup: RollupConfig::default(),
        }
    }
}

fn conv_default_retention_days() -> u32 {
    30
}
fn conv_default_poll_interval() -> u64 {
    300
}
fn conv_truthy() -> bool {
    true
}
fn conv_default_dedup() -> f64 {
    0.85
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactConfig {
    #[serde(default = "conv_truthy")]
    pub enabled_in_daemon: bool,
    #[serde(default = "compact_default_max_days")]
    pub max_days_per_run: u32,
    #[serde(default = "compact_default_model")]
    pub extractive_model: String,
    #[serde(default = "compact_default_model")]
    pub abstractive_model: String,
    #[serde(default = "compact_default_ollama_endpoint")]
    pub ollama_endpoint: String,
    #[serde(default = "compact_default_max_spans")]
    pub max_extractive_spans: u32,
    #[serde(default = "compact_default_max_words")]
    pub max_abstractive_words: u32,
    #[serde(default = "compact_default_chunk_tokens")]
    pub chunk_tokens: u32,
    #[serde(default = "compact_default_history_retain")]
    pub history_retain: u32,
    #[serde(default = "compact_default_cron")]
    pub daemon_cron: String,
    /// Per-stage backend override for extractive summarization.
    /// None = synthesize from legacy `extractive_model` + `ollama_endpoint`.
    #[serde(default)]
    pub extractive_backend: Option<BackendConfig>,
    /// Per-stage backend override for abstractive summarization.
    /// None = synthesize from legacy `abstractive_model` + `ollama_endpoint`.
    #[serde(default)]
    pub abstractive_backend: Option<BackendConfig>,
}

impl CompactConfig {
    /// Returns the effective backend for the extractive stage.
    /// Per-stage `extractive_backend` override wins; otherwise synthesize
    /// from legacy fields into an Ollama BackendConfig.
    ///
    /// CompactConfig has no per-stage timeout field, so synthesis bakes
    /// the conservative 120s default — matching the previously-hardcoded
    /// `Duration::from_secs(120)` at the call sites (byte-identical to
    /// the pre-trait OllamaClient construction).
    pub fn synthesize_extractive_backend(&self) -> BackendConfig {
        self.extractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                provider: "ollama".into(),
                model: self.extractive_model.clone(),
                endpoint: Some(self.ollama_endpoint.clone()),
                api_key_env: None,
                api_key_ref: None,
                timeout_secs: Some(120),
            })
    }

    /// Returns the effective backend for the abstractive stage.
    /// See `synthesize_extractive_backend` for the timeout rationale.
    pub fn synthesize_abstractive_backend(&self) -> BackendConfig {
        self.abstractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                provider: "ollama".into(),
                model: self.abstractive_model.clone(),
                endpoint: Some(self.ollama_endpoint.clone()),
                api_key_env: None,
                api_key_ref: None,
                timeout_secs: Some(120),
            })
    }

    /// Effective backend for the extractive stage. Override wins; otherwise
    /// inherit the smart slot. CompactConfig has no per-stage timeout field,
    /// so inheritance bakes the same conservative 120s the fabricated Ollama
    /// config used.
    pub fn effective_extractive_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.extractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(120),
                ..llm.to_backend_config()
            })
    }

    /// Effective backend for the abstractive stage. See
    /// `effective_extractive_backend` for the timeout rationale.
    pub fn effective_abstractive_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.abstractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(120),
                ..llm.to_backend_config()
            })
    }
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            enabled_in_daemon: true,
            max_days_per_run: compact_default_max_days(),
            extractive_model: compact_default_model(),
            abstractive_model: compact_default_model(),
            ollama_endpoint: compact_default_ollama_endpoint(),
            max_extractive_spans: compact_default_max_spans(),
            max_abstractive_words: compact_default_max_words(),
            chunk_tokens: compact_default_chunk_tokens(),
            history_retain: compact_default_history_retain(),
            daemon_cron: compact_default_cron(),
            extractive_backend: None,
            abstractive_backend: None,
        }
    }
}

fn compact_default_max_days() -> u32 {
    7
}
fn compact_default_model() -> String {
    DEFAULT_LOCAL_LLM_MODEL.into()
}
fn compact_default_ollama_endpoint() -> String {
    "http://localhost:11434".into()
}
fn compact_default_max_spans() -> u32 {
    20
}
fn compact_default_max_words() -> u32 {
    400
}
fn compact_default_chunk_tokens() -> u32 {
    6000
}
fn compact_default_history_retain() -> u32 {
    5
}
fn compact_default_cron() -> String {
    "0 0 3 * * * *".into()
}

// ── Rollup config (Phase 3.2, Task 1) ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupConfig {
    #[serde(default = "rollup_default_enabled")]
    pub enabled: bool,
    #[serde(default = "rollup_default_max_weeks")]
    pub max_weeks_per_run: u32,
    #[serde(default = "rollup_default_max_months")]
    pub max_months_per_run: u32,
    #[serde(default = "rollup_default_max_spans_week")]
    pub max_extractive_spans_per_week: u32,
    #[serde(default = "rollup_default_max_words_week")]
    pub max_abstractive_words_per_week: u32,
    #[serde(default = "rollup_default_max_spans_month")]
    pub max_extractive_spans_per_month: u32,
    #[serde(default = "rollup_default_max_words_month")]
    pub max_abstractive_words_per_month: u32,
    #[serde(default = "rollup_default_week_mmr")]
    pub week_mmr_threshold: f64,
    #[serde(default = "rollup_default_month_mmr")]
    pub month_mmr_threshold: f64,
    #[serde(default = "compact_default_model")]
    pub extractive_model: String,
    #[serde(default = "compact_default_model")]
    pub abstractive_model: String,
    #[serde(default = "compact_default_ollama_endpoint")]
    pub ollama_endpoint: String,
    /// Per-stage backend override for the extractive stage.
    /// None = inherit the smart slot (`config.llm`).
    #[serde(default)]
    pub extractive_backend: Option<BackendConfig>,
    /// Per-stage backend override for the abstractive stage.
    /// None = inherit the smart slot (`config.llm`).
    #[serde(default)]
    pub abstractive_backend: Option<BackendConfig>,
}

impl Default for RollupConfig {
    fn default() -> Self {
        Self {
            enabled: rollup_default_enabled(),
            max_weeks_per_run: rollup_default_max_weeks(),
            max_months_per_run: rollup_default_max_months(),
            max_extractive_spans_per_week: rollup_default_max_spans_week(),
            max_abstractive_words_per_week: rollup_default_max_words_week(),
            max_extractive_spans_per_month: rollup_default_max_spans_month(),
            max_abstractive_words_per_month: rollup_default_max_words_month(),
            week_mmr_threshold: rollup_default_week_mmr(),
            month_mmr_threshold: rollup_default_month_mmr(),
            extractive_model: compact_default_model(),
            abstractive_model: compact_default_model(),
            ollama_endpoint: compact_default_ollama_endpoint(),
            extractive_backend: None,
            abstractive_backend: None,
        }
    }
}

impl RollupConfig {
    /// Effective backend for the extractive stage. Override wins; otherwise
    /// inherit the smart slot with the same 120s budget the previously
    /// hardcoded inline config used (`summarize/rollup.rs`).
    pub fn effective_extractive_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.extractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(120),
                ..llm.to_backend_config()
            })
    }

    /// Effective backend for the abstractive stage.
    pub fn effective_abstractive_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.abstractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(120),
                ..llm.to_backend_config()
            })
    }
}

fn rollup_default_enabled() -> bool {
    true
}
fn rollup_default_max_weeks() -> u32 {
    4
}
fn rollup_default_max_months() -> u32 {
    2
}
fn rollup_default_max_spans_week() -> u32 {
    20
}
fn rollup_default_max_words_week() -> u32 {
    500
}
fn rollup_default_max_spans_month() -> u32 {
    20
}
fn rollup_default_max_words_month() -> u32 {
    700
}
fn rollup_default_week_mmr() -> f64 {
    0.85
}
fn rollup_default_month_mmr() -> f64 {
    0.82
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationsSources {
    #[serde(default = "conv_truthy")]
    pub claude_code: bool,
    #[serde(default = "conv_truthy")]
    pub cursor: bool,
    #[serde(default = "conv_truthy")]
    pub gemini: bool,
    #[serde(default)]
    pub aider: AiderSourceConfig,
}

impl Default for ConversationsSources {
    fn default() -> Self {
        Self {
            claude_code: true,
            cursor: true,
            gemini: true,
            aider: AiderSourceConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiderSourceConfig {
    #[serde(default = "conv_truthy")]
    pub enabled: bool,
    #[serde(default)]
    pub watched_dirs: Vec<String>,
}

impl Default for AiderSourceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            watched_dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationsFilter {
    #[serde(default = "conv_default_dedup")]
    pub dedup_threshold: f64,
    #[serde(default = "conv_truthy")]
    pub reject_heartbeat: bool,
    #[serde(default = "conv_truthy")]
    pub reject_system_restatement: bool,
}

impl Default for ConversationsFilter {
    fn default() -> Self {
        Self {
            dedup_threshold: conv_default_dedup(),
            reject_heartbeat: true,
            reject_system_restatement: true,
        }
    }
}

#[cfg(test)]
mod conversations_tests {
    use super::*;

    #[test]
    fn conversations_section_defaults() {
        let c = ConversationsConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.retention_days, 30);
        assert_eq!(c.poll_interval_secs, 300);
        assert!(c.sources.claude_code);
        assert!(c.sources.cursor);
        assert!(c.sources.gemini);
        assert!(c.sources.aider.enabled);
        assert!(c.sources.aider.watched_dirs.is_empty());
        assert_eq!(c.filter.dedup_threshold, 0.85);
        assert!(c.filter.reject_heartbeat);
        assert!(c.filter.reject_system_restatement);
    }

    #[test]
    fn parse_from_yaml_with_overrides() {
        let y = r#"
conversations:
  enabled: true
  retention_days: 45
  poll_interval_secs: 120
  sources:
    cursor: false
    aider:
      watched_dirs: ["~/Projects/a", "~/Projects/b"]
  filter:
    dedup_threshold: 0.9
"#;
        let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
        let conv: ConversationsConfig = serde_yaml::from_value(v["conversations"].clone()).unwrap();
        assert!(conv.enabled);
        assert_eq!(conv.retention_days, 45);
        assert_eq!(conv.poll_interval_secs, 120);
        assert!(conv.sources.claude_code); // defaulted true
        assert!(!conv.sources.cursor); // override
        assert!(conv.sources.gemini); // defaulted true
        assert_eq!(conv.sources.aider.watched_dirs.len(), 2);
        assert_eq!(conv.filter.dedup_threshold, 0.9);
        assert!(conv.filter.reject_heartbeat); // defaulted true
    }

    #[test]
    fn missing_conversations_section_is_fine() {
        let y = r#"
# No conversations section at all
foo: bar
"#;
        let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
        // Default when absent
        let conv: ConversationsConfig = v
            .get("conversations")
            .cloned()
            .map(|x| serde_yaml::from_value(x).unwrap_or_default())
            .unwrap_or_default();
        assert_eq!(conv.retention_days, 30);
    }

    #[test]
    fn compact_config_defaults() {
        let c = CompactConfig::default();
        assert!(c.enabled_in_daemon);
        assert_eq!(c.max_days_per_run, 7);
        assert_eq!(c.extractive_model, "qwen3.5:4b");
        assert_eq!(c.abstractive_model, "qwen3.5:4b");
        assert_eq!(c.ollama_endpoint, "http://localhost:11434");
        assert_eq!(c.max_extractive_spans, 20);
        assert_eq!(c.chunk_tokens, 6000);
        assert_eq!(c.history_retain, 5);
        assert_eq!(c.daemon_cron, "0 0 3 * * * *");
    }

    #[test]
    fn compact_parses_partial_overrides() {
        let y = r#"
conversations:
  compact:
    max_days_per_run: 3
    extractive_model: qwen3:4b
"#;
        let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
        let conv: ConversationsConfig = serde_yaml::from_value(v["conversations"].clone()).unwrap();
        assert_eq!(conv.compact.max_days_per_run, 3);
        assert_eq!(conv.compact.extractive_model, "qwen3:4b");
        assert!(conv.compact.enabled_in_daemon); // default preserved
        assert_eq!(conv.compact.abstractive_model, "qwen3.5:4b"); // default preserved
    }

    #[test]
    fn ask_config_defaults() {
        let c = AskConfig::default();
        assert_eq!(c.model, "qwen3.5:4b");
        assert_eq!(c.ollama_endpoint, "http://localhost:11434");
        assert_eq!(c.k_raw, 10);
        assert_eq!(c.escalation_threshold, 0.5);
        assert_eq!(c.mmr_threshold, 0.88);
        assert_eq!(c.max_context_tokens, 6000);
        assert_eq!(c.response_tokens, 1024);
        assert_eq!(c.timeout_secs, 120);
        assert_eq!(c.min_score, 0.35);
    }

    #[test]
    fn ask_config_mmr_threshold_default_is_cosine_scaled() {
        // Phase 3.1: default shifts from 0.85 (word-Jaccard) to 0.88 (cosine).
        let c = AskConfig::default();
        assert!(
            (c.mmr_threshold - 0.88).abs() < 1e-9,
            "expected 0.88, got {}",
            c.mmr_threshold
        );
    }

    #[test]
    fn rollup_config_defaults() {
        let c = RollupConfig::default();
        assert!(c.enabled);
        assert_eq!(c.max_weeks_per_run, 4);
        assert_eq!(c.max_months_per_run, 2);
        assert_eq!(c.max_extractive_spans_per_week, 20);
        assert_eq!(c.max_abstractive_words_per_week, 500);
        assert_eq!(c.max_extractive_spans_per_month, 20);
        assert_eq!(c.max_abstractive_words_per_month, 700);
        assert!((c.week_mmr_threshold - 0.85).abs() < 1e-9);
        assert!((c.month_mmr_threshold - 0.82).abs() < 1e-9);
        assert_eq!(c.extractive_model, "qwen3.5:4b");
        assert_eq!(c.abstractive_model, "qwen3.5:4b");
        assert_eq!(c.ollama_endpoint, "http://localhost:11434");
    }

    #[test]
    fn rollup_config_plumbed_into_conversations_config() {
        let c = ConversationsConfig::default();
        assert!(c.rollup.enabled);
    }

    #[test]
    fn ask_config_default_continue_history_turns_is_3() {
        let c = AskConfig::default();
        assert_eq!(c.continue_history_turns, 3);
    }

    #[test]
    fn ask_config_default_compress_hits_enabled_is_true() {
        let c = AskConfig::default();
        assert!(c.compress_hits_enabled);
    }

    #[test]
    fn ask_config_default_summarize_hits_enabled_is_true() {
        let c = AskConfig::default();
        assert!(c.summarize_hits_enabled);
    }

    #[test]
    fn ask_config_default_summarize_model_is_none() {
        let c = AskConfig::default();
        assert!(c.summarize_model.is_none());
    }

    #[test]
    fn ask_config_yaml_roundtrip_preserves_summarize_fields() {
        let y = r#"
conversations:
  ask:
    summarize_hits_enabled: false
    summarize_model: qwen3:4b
"#;
        let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
        let conv: ConversationsConfig = serde_yaml::from_value(v["conversations"].clone()).unwrap();
        assert!(!conv.ask.summarize_hits_enabled);
        assert_eq!(conv.ask.summarize_model.as_deref(), Some("qwen3:4b"));
    }

    #[test]
    fn ask_config_yaml_without_summarize_fields_uses_defaults() {
        // Phase 3.5 must be additive: an existing config.yaml with NO
        // summarize_* keys must still parse and default to enabled=true,
        // model=None.
        let y = r#"
conversations:
  ask:
    model: qwen3:14b
"#;
        let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
        let conv: ConversationsConfig = serde_yaml::from_value(v["conversations"].clone()).unwrap();
        assert!(conv.ask.summarize_hits_enabled);
        assert!(conv.ask.summarize_model.is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bundled_model_id_is_qwen35_2b() {
        assert_eq!(
            crate::config::DEFAULT_BUNDLED_MODEL_ID,
            "Qwen3.5-2B-MLX-4bit"
        );
    }

    #[test]
    fn nudge_config_defaults() {
        let c = NudgeConfig::default();
        assert!(c.enabled);
        assert_eq!(c.daily_cap, 3);
        assert_eq!(c.snooze_days, 7);
        assert_eq!(c.threshold, 3);
    }

    #[test]
    fn config_has_nudge_section_with_defaults() {
        let c: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(c.nudge.daily_cap, 3);
    }

    #[test]
    fn storage_config_default_is_lancedb() {
        let c = StorageConfig::default();
        assert_eq!(c.vector_backend, "lancedb");
        assert_eq!(c.qdrant_url, None);
        assert_eq!(c.qdrant_api_key_ref, None);
    }

    #[test]
    fn sources_global_config_has_sensible_defaults() {
        let c = SourcesGlobalConfig::default();
        assert_eq!(c.poll_interval_secs, 600);
        assert_eq!(c.max_chunks_per_sync, 10_000);
        assert_eq!(c.max_parallel_sources, 3);
        assert_eq!(c.default_weight, 1.0);
        assert_eq!(c.embedding_batch_size, 32);
    }

    #[test]
    fn config_default_has_storage_and_sources_global() {
        let c = Config::default();
        assert_eq!(c.storage.vector_backend, "lancedb");
        assert_eq!(c.sources_global.default_weight, 1.0);
    }

    #[test]
    fn config_loads_yaml_without_new_fields() {
        // Existing users' config.yaml won't mention storage or sources_global.
        // It must still parse.
        let yaml = r#"
embedding:
  provider: ollama
  model: test-model
  dimensions: 512
  ollama_endpoint: http://localhost:11434
"#;
        let c: Config = serde_yaml::from_str(yaml).expect("parses");
        assert_eq!(c.storage.vector_backend, "lancedb");
        assert_eq!(c.sources_global.max_parallel_sources, 3);
    }

    #[test]
    fn llm_config_to_backend_config_anthropic_passthrough() {
        let cfg = LlmConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            api_key_ref: None,
            openai_url: None,
        };
        let b = cfg.to_backend_config();
        assert_eq!(b.provider, "anthropic");
        assert_eq!(b.model, "claude-haiku-4-5");
        assert_eq!(b.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(b.endpoint, None);
        assert_eq!(b.timeout_secs, None);
    }

    #[test]
    fn llm_config_to_backend_config_openai_url_maps_to_endpoint() {
        let cfg = LlmConfig {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            api_key_env: None,
            api_key_ref: None,
            openai_url: Some("https://api.together.xyz/v1".into()),
        };
        let b = cfg.to_backend_config();
        assert_eq!(b.provider, "openai");
        assert_eq!(b.endpoint.as_deref(), Some("https://api.together.xyz/v1"));
        assert_eq!(b.api_key_env, None); // factory will fall back to OPENAI_API_KEY
    }

    #[test]
    fn llm_config_to_backend_config_ollama_openai_url_maps_to_endpoint() {
        let cfg = LlmConfig {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            api_key_env: None,
            api_key_ref: None,
            openai_url: Some("http://192.168.1.10:11434".into()),
        };
        let b = cfg.to_backend_config();
        assert_eq!(b.provider, "ollama");
        assert_eq!(b.endpoint.as_deref(), Some("http://192.168.1.10:11434"));
    }

    #[test]
    fn llm_config_to_backend_config_unknown_with_openai_url_aliases_to_openai() {
        // Historical LlmConfig allowed provider="custom" + openai_url to act as
        // an OpenAI-compatible passthrough. Preserve that by re-tagging as
        // "openai" so factory dispatches to OpenAIBackend.
        let cfg = LlmConfig {
            provider: "custom-name".into(),
            model: "some-model".into(),
            api_key_env: Some("CUSTOM_KEY".into()),
            api_key_ref: None,
            openai_url: Some("https://my-proxy.local/v1".into()),
        };
        let b = cfg.to_backend_config();
        assert_eq!(
            b.provider, "openai",
            "unknown provider + openai_url should alias to openai"
        );
        assert_eq!(b.endpoint.as_deref(), Some("https://my-proxy.local/v1"));
    }

    #[test]
    fn api_key_ref_roundtrips_and_defaults_none() {
        // Old YAML without the field still parses, field defaults to None.
        let b: BackendConfig = serde_yaml_ng::from_str("provider: anthropic\nmodel: m\n").unwrap();
        assert_eq!(b.api_key_ref, None);
        let l: LlmConfig = serde_yaml_ng::from_str("provider: anthropic\nmodel: m\n").unwrap();
        assert_eq!(l.api_key_ref, None);
        let e: EmbeddingConfig = serde_yaml_ng::from_str("provider: ollama\nmodel: m\n").unwrap();
        assert_eq!(e.api_key_ref, None);

        // Set → survives YAML round-trip and to_backend_config.
        let mut l2 = LlmConfig::default();
        l2.api_key_ref = Some("keychain:mur/anthropic".into());
        let y = serde_yaml_ng::to_string(&l2).unwrap();
        let l3: LlmConfig = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(l3.api_key_ref.as_deref(), Some("keychain:mur/anthropic"));
        assert_eq!(
            l3.to_backend_config().api_key_ref.as_deref(),
            Some("keychain:mur/anthropic")
        );
    }

    #[test]
    fn open_items_muted_parses_and_defaults_empty() {
        let c: Config = serde_yaml::from_str("open_items:\n  muted:\n    - inbox\n").unwrap();
        assert_eq!(c.open_items.muted, vec!["inbox".to_string()]);

        let d: Config = serde_yaml::from_str("llm:\n  model: x\n").unwrap();
        assert!(d.open_items.muted.is_empty(), "must default to no mutes");
    }

    /// Fail toward showing. A config that will not parse must yield an empty
    /// mute set, never a quiet, confident, incomplete list.
    #[test]
    fn unreadable_config_yields_no_mutes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(&path, "this: is: not: valid: yaml: [[[\n").unwrap();
        let cfg = Config::load_or_default(&path);
        assert!(
            cfg.open_items.muted.is_empty(),
            "a broken config must hide nothing"
        );

        // Same for a config that is simply absent.
        let missing = Config::load_or_default(&tmp.path().join("nope.yaml"));
        assert!(missing.open_items.muted.is_empty());
    }

    #[test]
    fn rollup_config_accepts_backend_overrides() {
        let yaml = r#"
enabled: true
extractive_backend:
  provider: openai
  model: Qwen3.5-4B-MLX-4bit
  endpoint: http://127.0.0.1:8000/v1
"#;
        let c: RollupConfig = serde_yaml_ng::from_str(yaml).expect("parses");
        let b = c.extractive_backend.expect("override present");
        assert_eq!(b.provider, "openai");
        assert_eq!(b.model, "Qwen3.5-4B-MLX-4bit");
        assert_eq!(b.endpoint.as_deref(), Some("http://127.0.0.1:8000/v1"));
        assert!(c.abstractive_backend.is_none());
    }
}

#[cfg(test)]
mod backend_config_tests {
    use super::*;

    #[test]
    fn default_is_ollama_qwen3() {
        let cfg = BackendConfig::default();
        assert_eq!(cfg.provider, "ollama");
        assert_eq!(cfg.model, "qwen3.5:4b");
        assert_eq!(cfg.endpoint, None);
        assert_eq!(cfg.api_key_env, None);
        assert_eq!(cfg.timeout_secs, None);
    }

    #[test]
    fn deserializes_anthropic_full() {
        let yaml = "\
provider: anthropic
model: claude-haiku-4-5
api_key_env: ANTHROPIC_API_KEY
timeout_secs: 60
";
        let cfg: BackendConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.model, "claude-haiku-4-5");
        assert_eq!(cfg.api_key_env, Some("ANTHROPIC_API_KEY".into()));
        assert_eq!(cfg.timeout_secs, Some(60));
        assert_eq!(cfg.endpoint, None);
    }

    #[test]
    fn deserializes_partial_fills_defaults() {
        let yaml = "provider: anthropic\nmodel: claude-sonnet-5\n";
        let cfg: BackendConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.model, "claude-sonnet-5");
        assert_eq!(cfg.api_key_env, None);
        assert_eq!(cfg.timeout_secs, None);
    }

    #[test]
    fn round_trips_through_yaml() {
        let original = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: Some("https://api.anthropic.com".into()),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            api_key_ref: None,
            timeout_secs: Some(60),
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        let parsed: BackendConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn skills_config_curation_gate_defaults_on() {
        let c = SkillsConfig::default();
        assert!(c.require_human_curation_before_stable);
    }
}

/// Configuration for the daemon-side sleep cycle (idle background learning).
///
/// Skill injection configuration (M2 — runtime injection).
///
/// Whether the `mur-dev` discipline hub appears in the session-start learning
/// index on the AI-tool (CLI hook) surface. Runtime injection for MUR agents
/// is never affected by this setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DevDisciplineIndex {
    /// Suppress the hub when a superpowers plugin install is detected (default).
    #[default]
    Auto,
    /// Always list the hub, even when superpowers is installed.
    Always,
    /// Never list the hub on the CLI surface.
    Never,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    pub max_skills_in_prompt: usize,
    pub max_total_tokens: usize,
    pub priority_order: Vec<String>,
    pub adaptive: Option<AdaptiveSkillsConfig>,

    /// When true (default), LLM-authored skills cannot auto-promote past
    /// `Emerging` until a human curates them (amendment A1). Set false to
    /// let LLM-extracted skills promote on run stats alone.
    #[serde(default = "default_require_human_curation")]
    pub require_human_curation_before_stable: bool,

    /// Lifecycle scoring thresholds (W3b-P4). All fields default to the
    /// compile-time constants in `mur_common::skill::lifecycle` so existing
    /// deployments see no behaviour change without an explicit config entry.
    #[serde(default)]
    pub lifecycle: SkillLifecycleConfig,

    /// Daily daemon auto-upgrade of origin-stamped (registry-installed)
    /// skills (`mur-daemon` `skill_upgrade_tick`). Non-destructive: never
    /// overwrites a locally-modified skill (origin hash drift blocks it).
    /// Defaults to `true`.
    #[serde(default = "default_auto_upgrade")]
    pub auto_upgrade: bool,

    /// See [`DevDisciplineIndex`]. Key: `skills.dev_discipline_index`.
    #[serde(default)]
    pub dev_discipline_index: DevDisciplineIndex,
}

fn default_require_human_curation() -> bool {
    true
}

fn default_auto_upgrade() -> bool {
    true
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            max_skills_in_prompt: 5,
            max_total_tokens: 2000,
            priority_order: vec!["agent".into(), "global".into()],
            adaptive: Some(AdaptiveSkillsConfig::default()),
            require_human_curation_before_stable: default_require_human_curation(),
            lifecycle: SkillLifecycleConfig::default(),
            auto_upgrade: default_auto_upgrade(),
            dev_discipline_index: DevDisciplineIndex::default(),
        }
    }
}

/// Per-skill lifecycle scoring thresholds.
///
/// Stored under `skill.lifecycle.*` in `~/.mur/config.yaml`.
/// All fields are optional on disk — missing keys fall back to the
/// compile-time defaults so a partial config is always valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillLifecycleConfig {
    // ── Promotion thresholds (must be exceeded) ──────────────────────────
    pub promote_draft_uses: u64,
    pub promote_emerging_uses: u64,
    pub promote_emerging_success_rate: f64,
    pub promote_emerging_age_days: i64,
    pub promote_stable_uses: u64,
    pub promote_stable_success_rate: f64,
    pub promote_stable_age_days: i64,

    // ── Demotion thresholds (must drop below) ────────────────────────────
    pub demote_emerging_uses: u64,
    pub demote_emerging_success_rate: f64,
    pub demote_stable_uses: u64,
    pub demote_stable_success_rate: f64,
    pub deprecated_success_rate: f64,
    pub deprecated_no_success_days: i64,

    // ── Auto-archive thresholds ───────────────────────────────────────────
    pub auto_archive_confidence: f64,
    pub auto_archive_age_days: i64,

    // ── P4: broken fast-path ─────────────────────────────────────────────
    /// Number of consecutive `Execution` events with `env_class == "workflow"`
    /// that immediately triggers a `Deprecated` transition, bypassing the
    /// normal scoring path. Set to 0 to disable the fast-path.
    pub broken_workflow_streak: u32,

    // ── P4: archived hard-delete ─────────────────────────────────────────
    /// Days a skill must remain in `Archived` state before `mur skill sweep`
    /// transitions it to `Destroyed` and removes its directory from disk.
    /// Set to 0 to disable hard-delete.
    pub archive_destroy_grace_days: i64,
}

impl Default for SkillLifecycleConfig {
    fn default() -> Self {
        Self {
            promote_draft_uses: 3,
            promote_emerging_uses: 10,
            promote_emerging_success_rate: 0.6,
            promote_emerging_age_days: 7,
            promote_stable_uses: 30,
            promote_stable_success_rate: 0.8,
            promote_stable_age_days: 30,
            demote_emerging_uses: 8,
            demote_emerging_success_rate: 0.55,
            demote_stable_uses: 25,
            demote_stable_success_rate: 0.75,
            deprecated_success_rate: 0.3,
            deprecated_no_success_days: 90,
            auto_archive_confidence: 0.10,
            auto_archive_age_days: 180,
            broken_workflow_streak: 3,
            archive_destroy_grace_days: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdaptiveSkillsConfig {
    pub context_fill_decay: f64,
    pub min_remaining_context_ratio: f64,
    pub recent_fire_boost_turns: usize,
    /// Model max context window in tokens. Used to compute
    /// `context_fill_ratio = cumulative_input_tokens / model_max_context_tokens`.
    /// Default 200_000 (Claude 3.5/4.x).
    pub model_max_context_tokens: u64,
}

impl Default for AdaptiveSkillsConfig {
    fn default() -> Self {
        Self {
            context_fill_decay: 1.5,
            min_remaining_context_ratio: 0.20,
            recent_fire_boost_turns: 5,
            model_max_context_tokens: 200_000,
        }
    }
}

/// When enabled, the daemon fires a consolidation pipeline after the user has been
/// idle for `idle_threshold_minutes` minutes (default 15). Opt-in only — off by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleepCycleConfig {
    /// Master switch. False by default (opt-in).
    #[serde(default)]
    pub enabled: bool,

    /// Minutes of idle (no events) before triggering the daemon sleep cycle.
    #[serde(default = "default_idle_threshold_minutes")]
    pub idle_threshold_minutes: u64,

    /// Minutes of agent idle before the agent-side cycle fires (outbox flush + snapshot pull).
    #[serde(default = "default_agent_idle_minutes")]
    pub agent_idle_minutes: u64,
}

fn default_idle_threshold_minutes() -> u64 {
    15
}

fn default_agent_idle_minutes() -> u64 {
    5
}

impl Default for SleepCycleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            idle_threshold_minutes: default_idle_threshold_minutes(),
            agent_idle_minutes: default_agent_idle_minutes(),
        }
    }
}

// ── Nudge config ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NudgeConfig {
    /// Master switch. Default on — Phase 2 companion surface is live.
    #[serde(default = "default_nudge_enabled")]
    pub enabled: bool,
    #[serde(default = "default_nudge_daily_cap")]
    pub daily_cap: u32,
    #[serde(default = "default_nudge_snooze_days")]
    pub snooze_days: u32,
    #[serde(default = "default_nudge_threshold")]
    pub threshold: usize,
}

fn default_nudge_enabled() -> bool {
    true
}
fn default_nudge_daily_cap() -> u32 {
    3
}
fn default_nudge_snooze_days() -> u32 {
    7
}
fn default_nudge_threshold() -> usize {
    3
}

impl Default for NudgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            daily_cap: default_nudge_daily_cap(),
            snooze_days: default_nudge_snooze_days(),
            threshold: default_nudge_threshold(),
        }
    }
}

// ── Ambient capture & harvest (2026-06-11 spec) ────────────────────

/// Ambient session capture (spec 2026-06-11-mur-ambient-capture-and-harvest §3.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCfg {
    /// "ambient" (hooks always record) | "manual" (legacy `mur session in` gate) | "off"
    #[serde(default = "default_capture_mode")]
    pub capture: String,
    /// Recordings older than this many days are removed by `mur session gc`.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

impl Default for SessionCfg {
    fn default() -> Self {
        Self {
            capture: default_capture_mode(),
            retention_days: default_retention_days(),
        }
    }
}

fn default_capture_mode() -> String {
    "ambient".to_string()
}
fn default_retention_days() -> u32 {
    14
}

/// Harvest gate + token-budget defenses (spec §3.2, §3.7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarvestCfg {
    /// Run the heuristic gate automatically (from `mur session gc` / `mur out`).
    #[serde(default = "default_harvest_enabled")]
    pub auto_gate: bool,
    /// "local-first" | "cloud" | "off" — W1/W2 only persist this; LLM wiring lands with v2 P5a.
    #[serde(default = "default_harvest_llm")]
    pub llm: String,
    /// Gate thresholds — a session must clear at least one of these (see harvest::gate).
    #[serde(default = "default_min_events")]
    pub min_events: usize,
    #[serde(default = "default_min_user_turns")]
    pub min_user_turns: usize,
    #[serde(default = "default_min_duration_secs")]
    pub min_duration_secs: i64,
    /// A session is considered ended when its last event is older than this.
    #[serde(default = "default_idle_minutes")]
    pub idle_minutes: i64,
    /// Ceilings — past these a recording is a session, not a procedure (#781).
    /// A session marked with `mur in` bypasses both.
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
    #[serde(default = "default_max_duration_secs")]
    pub max_duration_secs: i64,
    /// §3.7 hard caps (persisted now; enforced when the LLM extract path lands in v2 P5a).
    #[serde(default = "default_max_llm_calls_per_day")]
    pub max_llm_calls_per_day: u32,
    #[serde(default = "default_max_extract_input_tokens")]
    pub max_extract_input_tokens: usize,
    /// §3.8 tier-1: one-line pending-proposals hint at SessionStart.
    #[serde(default = "default_harvest_enabled")]
    pub session_start_hint: bool,
    /// Step-skeleton Jaccard similarity at/above which a proposal becomes a merge suggestion.
    /// Doubles as the "same procedure?" test for the recurrence index (#783).
    #[serde(default = "default_similarity_merge_threshold")]
    pub similarity_merge_threshold: f32,
    /// A procedure is something done more than once (#783): a session's skeleton
    /// must have been seen this many times before it becomes a proposal.
    /// A session marked with `mur in` bypasses it.
    #[serde(default = "default_min_occurrences")]
    pub min_occurrences: usize,
}

impl Default for HarvestCfg {
    fn default() -> Self {
        serde_yaml::from_str("{}").expect("HarvestCfg defaults")
    }
}

fn default_harvest_enabled() -> bool {
    true
}
fn default_harvest_llm() -> String {
    "local-first".to_string()
}
fn default_min_events() -> usize {
    5
}
fn default_min_user_turns() -> usize {
    2
}
fn default_min_duration_secs() -> i64 {
    120
}
fn default_idle_minutes() -> i64 {
    30
}
/// Above ~20 distinct commands a recording reads as a transcript, not a
/// procedure a human would write down. Measured against a real 38-proposal
/// inbox: everything plausible sat below it, nothing accepted sat above (#781).
fn default_max_steps() -> usize {
    20
}
/// 30 minutes. Long enough for a real deploy/release procedure including waits,
/// short enough to exclude debugging sessions (#781).
fn default_max_duration_secs() -> i64 {
    1800
}
fn default_max_llm_calls_per_day() -> u32 {
    10
}
fn default_max_extract_input_tokens() -> usize {
    12000
}
fn default_similarity_merge_threshold() -> f32 {
    0.6
}
/// Twice. The minimum that can distinguish "did it again" from "did it" — a
/// higher bar would silently discard real routines while the index is young (#783).
fn default_min_occurrences() -> usize {
    2
}

// ── M7a: Cross-agent observability ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CrossAgentConfig {
    #[serde(default = "default_half_life_days")]
    pub fitness_half_life_days: u32,
    #[serde(default = "default_fitness_floor")]
    pub fitness_floor: f64,
}

fn default_half_life_days() -> u32 {
    7
}
fn default_fitness_floor() -> f64 {
    0.1
}

impl Default for CrossAgentConfig {
    fn default() -> Self {
        Self {
            fitness_half_life_days: default_half_life_days(),
            fitness_floor: default_fitness_floor(),
        }
    }
}

// ── M6c: LLM-augmented skill maintenance ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillLlmConfig {
    /// Per-call output token cap.
    #[serde(default = "default_per_call_token_cap")]
    pub per_call_token_cap: u32,

    /// Per-day USD cap for all maintenance LLM calls.
    #[serde(default = "default_per_day_usd_cap")]
    pub per_day_usd_cap: f64,

    /// Cache TTL in days.
    #[serde(default = "default_cache_ttl_days")]
    pub cache_ttl_days: u32,

    /// Optional explicit model key override. When `None`, role resolution picks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
}

fn default_per_call_token_cap() -> u32 {
    1500
}
fn default_per_day_usd_cap() -> f64 {
    0.50
}
fn default_cache_ttl_days() -> u32 {
    30
}

impl Default for SkillLlmConfig {
    fn default() -> Self {
        Self {
            per_call_token_cap: default_per_call_token_cap(),
            per_day_usd_cap: default_per_day_usd_cap(),
            cache_ttl_days: default_cache_ttl_days(),
            model_ref: None,
        }
    }
}
#[cfg(test)]
mod per_stage_backend_tests {
    use super::*;

    #[test]
    fn legacy_compact_config_has_no_per_stage_overrides() {
        let yaml = "\
extractive_model: qwen3:14b
abstractive_model: qwen3:14b
ollama_endpoint: http://localhost:11434
";
        let cfg: CompactConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.extractive_backend.is_none());
        assert!(cfg.abstractive_backend.is_none());
        assert_eq!(cfg.extractive_model, "qwen3:14b");
        assert_eq!(cfg.abstractive_model, "qwen3:14b");
        assert_eq!(cfg.ollama_endpoint, "http://localhost:11434");
    }

    #[test]
    fn legacy_ask_config_has_no_per_stage_overrides() {
        let yaml = "model: qwen3:14b\nollama_endpoint: http://localhost:11434\n";
        let cfg: AskConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.backend.is_none());
        assert!(cfg.rewriter_backend.is_none());
        assert_eq!(cfg.model, "qwen3:14b");
    }

    #[test]
    fn compact_extractive_backend_override_parses() {
        let yaml = "\
extractive_backend:
  provider: anthropic
  model: claude-haiku-4-5
  api_key_env: ANTHROPIC_API_KEY
abstractive_model: qwen3:14b
";
        let cfg: CompactConfig = serde_yaml::from_str(yaml).unwrap();
        let extractive = cfg
            .extractive_backend
            .as_ref()
            .expect("override should parse");
        assert_eq!(extractive.provider, "anthropic");
        assert_eq!(extractive.model, "claude-haiku-4-5");
        assert!(cfg.abstractive_backend.is_none());
    }

    #[test]
    fn ask_rewriter_backend_can_override_to_local_while_answer_is_cloud() {
        let yaml = "\
backend:
  provider: anthropic
  model: claude-sonnet-5
  api_key_env: ANTHROPIC_API_KEY
rewriter_backend:
  provider: ollama
  model: llama3.2:3b
";
        let cfg: AskConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.backend.as_ref().unwrap().provider, "anthropic");
        assert_eq!(cfg.rewriter_backend.as_ref().unwrap().provider, "ollama");
    }

    #[test]
    fn synthesize_legacy_to_backend_config_for_compact_extractive() {
        let yaml = "\
extractive_model: qwen3:14b
ollama_endpoint: http://192.168.1.10:11434
";
        let cfg: CompactConfig = serde_yaml::from_str(yaml).unwrap();
        let synth = cfg.synthesize_extractive_backend();
        assert_eq!(synth.provider, "ollama");
        assert_eq!(synth.model, "qwen3:14b");
        assert_eq!(synth.endpoint.as_deref(), Some("http://192.168.1.10:11434"));
        assert_eq!(synth.api_key_env, None);
    }

    #[test]
    fn synthesize_legacy_to_backend_config_for_ask() {
        let yaml = "model: qwen3:14b\nollama_endpoint: http://localhost:11434\n";
        let cfg: AskConfig = serde_yaml::from_str(yaml).unwrap();
        let synth = cfg.synthesize_backend();
        assert_eq!(synth.provider, "ollama");
        assert_eq!(synth.model, "qwen3:14b");
        assert_eq!(synth.endpoint.as_deref(), Some("http://localhost:11434"));
    }

    #[test]
    fn synthesize_rewriter_uses_legacy_ollama_when_no_rewriter_override() {
        // Rewriter no longer falls through to synthesize_backend() when
        // `rewriter_backend` is unset (see I2 fix in P3 task 1). It now
        // always synthesizes its own ollama BackendConfig over the legacy
        // model + endpoint with `rewriter_timeout_secs` baked in, so a
        // slow rewriter call doesn't burn the full ask budget. The
        // per-stage `ask.backend` override therefore does NOT propagate to
        // the rewriter — set `ask.rewriter_backend` explicitly if you want
        // a non-Ollama rewriter.
        let yaml = "\
backend:
  provider: anthropic
  model: claude-sonnet-5
  api_key_env: ANTHROPIC_API_KEY
";
        let cfg: AskConfig = serde_yaml::from_str(yaml).unwrap();
        let rewriter = cfg.synthesize_rewriter_backend();
        assert_eq!(rewriter.provider, "ollama");
        assert_eq!(rewriter.model, ask_default_model());
        assert_eq!(
            rewriter.timeout_secs,
            Some(ask_default_rewriter_timeout() as u64)
        );
    }

    #[test]
    fn ask_synthesize_backend_inherits_timeout_secs_from_legacy_field() {
        let cfg = AskConfig {
            timeout_secs: 45,
            ..AskConfig::default()
        };
        let b = cfg.synthesize_backend();
        assert_eq!(
            b.timeout_secs,
            Some(45),
            "synthesize_backend() must propagate ask.timeout_secs into the synthesized BackendConfig"
        );
    }

    #[test]
    fn ask_synthesize_backend_does_not_override_explicit_per_stage_timeout() {
        let mut cfg = AskConfig {
            timeout_secs: 45,
            ..AskConfig::default()
        };
        cfg.backend = Some(BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            api_key_ref: None,
            timeout_secs: Some(10),
        });
        let b = cfg.synthesize_backend();
        assert_eq!(
            b.timeout_secs,
            Some(10),
            "explicit per-stage timeout_secs must NOT be overridden by ask.timeout_secs"
        );
    }

    #[test]
    fn ask_synthesize_rewriter_backend_uses_rewriter_timeout_secs_when_synthesizing() {
        let cfg = AskConfig {
            timeout_secs: 120,
            rewriter_timeout_secs: 8,
            ..AskConfig::default()
        };
        let b = cfg.synthesize_rewriter_backend();
        assert_eq!(
            b.timeout_secs,
            Some(8),
            "rewriter synthesis must use rewriter_timeout_secs (not the answer-call timeout)"
        );
    }

    #[test]
    fn ask_synthesize_rewriter_backend_does_not_override_explicit_per_stage_timeout() {
        let mut cfg = AskConfig {
            rewriter_timeout_secs: 8,
            ..AskConfig::default()
        };
        cfg.rewriter_backend = Some(BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            api_key_ref: None,
            timeout_secs: Some(30),
        });
        let b = cfg.synthesize_rewriter_backend();
        assert_eq!(
            b.timeout_secs,
            Some(30),
            "explicit per-stage rewriter timeout_secs must NOT be overridden by ask.rewriter_timeout_secs"
        );
    }

    #[test]
    fn compact_synthesize_extractive_backend_inherits_default_timeout_when_no_override() {
        // CompactConfig has no per-stage timeout field — extractive synthesis
        // should fall back to the conservative 120s default.
        let cfg = CompactConfig::default();
        let b = cfg.synthesize_extractive_backend();
        assert_eq!(
            b.timeout_secs,
            Some(120),
            "compact synthesis without per-stage override must produce 120s timeout"
        );
    }

    #[test]
    fn compact_synthesize_abstractive_backend_inherits_default_timeout_when_no_override() {
        let cfg = CompactConfig::default();
        let b = cfg.synthesize_abstractive_backend();
        assert_eq!(b.timeout_secs, Some(120));
    }

    fn omlx_llm() -> LlmConfig {
        LlmConfig {
            provider: "omlx".into(),
            model: "Qwen3.5-4B-MLX-4bit".into(),
            api_key_env: None,
            api_key_ref: Some("env:OMLX_API_KEY".into()),
            openai_url: Some("http://127.0.0.1:8000/v1".into()),
        }
    }

    #[test]
    fn ask_without_override_inherits_smart_slot_and_maps_omlx_to_openai() {
        let ask = AskConfig::default();
        let b = ask.effective_backend(&omlx_llm());
        assert_eq!(b.provider, "openai");
        assert_eq!(b.model, "Qwen3.5-4B-MLX-4bit");
        assert_eq!(b.endpoint.as_deref(), Some("http://127.0.0.1:8000/v1"));
        assert_eq!(b.api_key_ref.as_deref(), Some("env:OMLX_API_KEY"));
        // stage timeout is baked in, not left to the factory's 120s default
        assert_eq!(b.timeout_secs, Some(ask.timeout_secs as u64));
    }

    #[test]
    fn ask_rewriter_inherits_its_own_shorter_timeout_not_the_answer_one() {
        let ask = AskConfig::default();
        let b = ask.effective_rewriter_backend(&omlx_llm());
        assert_eq!(b.timeout_secs, Some(ask.rewriter_timeout_secs as u64));
        assert_ne!(b.timeout_secs, Some(ask.timeout_secs as u64));
    }

    #[test]
    fn explicit_override_wins_over_the_smart_slot() {
        let mut ask = AskConfig::default();
        ask.backend = Some(BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: Some(42),
        });
        let b = ask.effective_backend(&omlx_llm());
        assert_eq!(b.provider, "anthropic");
        assert_eq!(b.timeout_secs, Some(42));
    }

    #[test]
    fn compact_and_rollup_inherit_smart_slot_with_the_120s_budget() {
        let llm = omlx_llm();
        for b in [
            CompactConfig::default().effective_extractive_backend(&llm),
            CompactConfig::default().effective_abstractive_backend(&llm),
            RollupConfig::default().effective_extractive_backend(&llm),
            RollupConfig::default().effective_abstractive_backend(&llm),
        ] {
            assert_eq!(b.provider, "openai");
            assert_eq!(b.endpoint.as_deref(), Some("http://127.0.0.1:8000/v1"));
            assert_eq!(b.timeout_secs, Some(120));
        }
    }

    #[test]
    fn rollup_override_is_honored() {
        let mut r = RollupConfig::default();
        r.abstractive_backend = Some(BackendConfig {
            provider: "ollama".into(),
            model: "qwen3:4b".into(),
            endpoint: Some("http://box.local:11434".into()),
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: None,
        });
        let b = r.effective_abstractive_backend(&omlx_llm());
        assert_eq!(b.provider, "ollama");
        assert_eq!(b.endpoint.as_deref(), Some("http://box.local:11434"));
    }
}

#[cfg(test)]
mod skills_config_tests {
    use super::*;

    #[test]
    fn empty_yaml_hydrates_defaults() {
        let cfg: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(cfg.skills.max_skills_in_prompt, 5);
        assert_eq!(cfg.skills.max_total_tokens, 2000);
        assert!(cfg.skills.adaptive.is_some());
    }

    #[test]
    fn load_or_default_missing_file_returns_default() {
        let cfg = Config::load_or_default(std::path::Path::new("/nonexistent/config.yaml"));
        assert_eq!(cfg.skills.max_skills_in_prompt, 5);
    }

    #[test]
    fn dev_discipline_index_defaults_auto_and_parses() {
        use crate::config::DevDisciplineIndex;
        let cfg: Config = serde_yaml_ng::from_str("").unwrap_or_default();
        assert_eq!(cfg.skills.dev_discipline_index, DevDisciplineIndex::Auto);
        let cfg: Config =
            serde_yaml_ng::from_str("skills:\n  dev_discipline_index: never\n").unwrap();
        assert_eq!(cfg.skills.dev_discipline_index, DevDisciplineIndex::Never);
        let cfg: Config =
            serde_yaml_ng::from_str("skills:\n  dev_discipline_index: always\n").unwrap();
        assert_eq!(cfg.skills.dev_discipline_index, DevDisciplineIndex::Always);
    }
}

#[cfg(test)]
mod ambient_capture_cfg_tests {
    use super::*;

    #[test]
    fn session_and_harvest_defaults() {
        let cfg: Config = serde_yaml::from_str("{}").unwrap();
        assert_eq!(cfg.session.capture, "ambient");
        assert_eq!(cfg.session.retention_days, 14);
        assert!(cfg.harvest.auto_gate);
        assert_eq!(cfg.harvest.llm, "local-first");
        assert_eq!(cfg.harvest.min_events, 5);
        assert_eq!(cfg.harvest.min_user_turns, 2);
        assert_eq!(cfg.harvest.min_duration_secs, 120);
        assert_eq!(cfg.harvest.idle_minutes, 30);
        assert_eq!(cfg.harvest.max_llm_calls_per_day, 10);
        assert_eq!(cfg.harvest.max_extract_input_tokens, 12000);
        assert!(cfg.harvest.session_start_hint);
        assert!((cfg.harvest.similarity_merge_threshold - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn session_capture_override_parses() {
        let cfg: Config =
            serde_yaml::from_str("session:\n  capture: off\n  retention_days: 3\n").unwrap();
        assert_eq!(cfg.session.capture, "off");
        assert_eq!(cfg.session.retention_days, 3);
    }
}

#[cfg(test)]
mod cc_proxy_cfg_tests {
    use super::*;

    #[test]
    fn defaults_to_local_cc_proxy_enabled() {
        let cfg: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(cfg.cc_proxy.url, "http://127.0.0.1:8088");
        assert!(cfg.cc_proxy.enabled);
    }

    #[test]
    fn url_and_enabled_override_parse() {
        let cfg: Config =
            serde_yaml_ng::from_str("cc_proxy:\n  url: http://127.0.0.1:9999\n  enabled: false\n")
                .unwrap();
        assert_eq!(cfg.cc_proxy.url, "http://127.0.0.1:9999");
        assert!(!cfg.cc_proxy.enabled);
    }

    #[test]
    fn partial_section_keeps_other_default() {
        // Only `enabled` given → url stays at the default.
        let cfg: Config = serde_yaml_ng::from_str("cc_proxy:\n  enabled: false\n").unwrap();
        assert_eq!(cfg.cc_proxy.url, "http://127.0.0.1:8088");
        assert!(!cfg.cc_proxy.enabled);
    }
}

#[cfg(test)]
mod model_switch_config_tests {
    use super::*;

    #[test]
    fn model_switch_config_defaults_and_omitted_block() {
        // Omitted `models:` block deserializes to defaults.
        let cfg: Config = serde_yaml::from_str("{}").unwrap();
        assert_eq!(cfg.models.default, None);
        assert!(cfg.models.fallback_chain.is_empty());
        assert_eq!(cfg.models.retry.max_retries, DEFAULT_MAX_RETRIES);
        assert_eq!(cfg.models.retry.backoff_base_ms, DEFAULT_BACKOFF_BASE_MS);
        assert_eq!(cfg.models.retry.cooldown_secs, DEFAULT_COOLDOWN_SECS);
        assert!(!cfg.models.routing.enabled);

        // A populated block round-trips.
        let yaml = "models:\n  default: claude_sonnet\n  fallback_chain: [claude_sonnet, deepseek_v4_pro]\n  routing:\n    enabled: true\n    cheap: deepseek_v4_flash\n    frontier: claude_opus\n    threshold_input_tokens: 1500\n";
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.models.default.as_deref(), Some("claude_sonnet"));
        assert_eq!(
            cfg.models.fallback_chain,
            vec!["claude_sonnet", "deepseek_v4_pro"]
        );
        assert!(cfg.models.routing.enabled);
        assert_eq!(cfg.models.routing.threshold_input_tokens, Some(1500));
    }

    #[test]
    fn smart_config_defaults_on_with_autopick() {
        let cfg: Config = serde_yaml::from_str("{}").unwrap();
        assert!(cfg.models.smart.enabled); // default ON
        assert_eq!(cfg.models.smart.cheap, None); // auto-pick
        assert_eq!(
            cfg.models.smart.max_escalations,
            DEFAULT_SMART_MAX_ESCALATIONS
        );
    }
}
