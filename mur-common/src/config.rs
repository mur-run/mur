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
}
