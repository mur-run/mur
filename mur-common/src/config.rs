use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Global MUR configuration (~/.mur/config.yaml)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub embedding: EmbeddingConfig,

    #[serde(default)]
    pub llm: LlmConfig,

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
            openai_url: None,
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
    /// Model name as the provider sees it ("claude-haiku-4-5", "qwen3:14b", …).
    pub model: String,
    /// Provider endpoint. None = provider default
    /// (ollama: http://localhost:11434, anthropic: https://api.anthropic.com).
    pub endpoint: Option<String>,
    /// Env var holding the API key. None = no auth (ollama).
    pub api_key_env: Option<String>,
    /// Per-call timeout in seconds. None = 120s.
    pub timeout_secs: Option<u64>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            endpoint: None,
            api_key_env: None,
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
    "claude-opus-4-6".to_string()
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
                timeout_secs: Some(self.rewriter_timeout_secs as u64),
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
    "qwen3:14b".into()
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
                timeout_secs: Some(120),
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
    "qwen3:14b".into()
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
        }
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
        assert_eq!(c.extractive_model, "qwen3:14b");
        assert_eq!(c.abstractive_model, "qwen3:14b");
        assert_eq!(c.ollama_endpoint, "http://localhost:11434");
        assert_eq!(c.max_extractive_spans, 20);
        assert_eq!(c.max_abstractive_words, 400);
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
        assert_eq!(conv.compact.abstractive_model, "qwen3:14b"); // default preserved
    }

    #[test]
    fn ask_config_defaults() {
        let c = AskConfig::default();
        assert_eq!(c.model, "qwen3:14b");
        assert_eq!(c.ollama_endpoint, "http://localhost:11434");
        assert_eq!(c.k_summary, 5);
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
        assert_eq!(c.extractive_model, "qwen3:14b");
        assert_eq!(c.abstractive_model, "qwen3:14b");
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
}

#[cfg(test)]
mod backend_config_tests {
    use super::*;

    #[test]
    fn default_is_ollama_qwen3() {
        let cfg = BackendConfig::default();
        assert_eq!(cfg.provider, "ollama");
        assert_eq!(cfg.model, "qwen3:14b");
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
        let yaml = "provider: anthropic\nmodel: claude-sonnet-4-6\n";
        let cfg: BackendConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.model, "claude-sonnet-4-6");
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
            timeout_secs: Some(60),
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        let parsed: BackendConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, original);
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
  model: claude-sonnet-4-6
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
  model: claude-sonnet-4-6
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
}
