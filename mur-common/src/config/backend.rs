use super::*;

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

    /// Ollama endpoint. `None` for every non-Ollama provider — the OpenAI
    /// path uses `openai_url`. Kept out of the serialized document when
    /// unset so it stops reappearing in configs that never use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_endpoint: Option<String>,

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
            ollama_endpoint: Some(default_ollama_endpoint()),
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

    /// Injection slots reserved for notes when mature skills would otherwise
    /// fill every seat (memory federation P1). 0 disables the reservation.
    #[serde(default = "default_reserved_note_slots")]
    pub reserved_note_slots: usize,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            max_patterns: default_max_patterns(),
            max_tokens: default_max_tokens(),
            min_score: default_min_score(),
            mmr_threshold: default_mmr_threshold(),
            reserved_note_slots: default_reserved_note_slots(),
        }
    }
}

fn default_reserved_note_slots() -> usize {
    1
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
    DEFAULT_OLLAMA_ENDPOINT.to_string()
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

#[cfg(test)]
mod per_stage_backend_tests {
    use super::*;

    #[test]
    fn compact_extractive_backend_override_parses() {
        let yaml = "\
extractive_backend:
  provider: anthropic
  model: claude-haiku-4-5
  api_key_env: ANTHROPIC_API_KEY
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
    fn rewriter_falls_through_to_answer_stage_backend_before_the_smart_slot() {
        // C1 regression test: effective_rewriter_backend must follow the
        // answer stage's `backend` when `rewriter_backend` is unset, NOT
        // fall straight through to the smart slot (`llm`) — the rewriter is
        // not an independent pinning point, only an independent timeout.
        // `llm` below uses a distinctly different provider ("omlx", which
        // to_backend_config() maps to "openai") than `cfg.backend`
        // ("anthropic"), so this test cannot pass by coincidentally landing
        // on the same provider from either source: if the fix regresses to
        // falling through to `llm`, `rewriter.provider` comes back
        // "openai" and the first assertion fails.
        let cfg = AskConfig {
            backend: Some(BackendConfig {
                provider: "anthropic".into(),
                model: "claude-sonnet-5".into(),
                endpoint: None,
                api_key_env: Some("ANTHROPIC_API_KEY".into()),
                api_key_ref: None,
                timeout_secs: None,
            }),
            ..Default::default()
        };
        let rewriter = cfg.effective_rewriter_backend(&omlx_llm());
        assert_eq!(rewriter.provider, "anthropic");
        assert_eq!(rewriter.model, "claude-sonnet-5");
        assert_eq!(
            rewriter.timeout_secs,
            Some(cfg.rewriter_timeout_secs as u64),
            "rewriter must keep its own tighter timeout even while following the answer stage's backend"
        );
    }

    #[test]
    fn rewriter_explicit_override_timeout_wins_over_rewriter_timeout_secs() {
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
        let b = cfg.effective_rewriter_backend(&omlx_llm());
        assert_eq!(
            b.timeout_secs,
            Some(30),
            "explicit per-stage rewriter_backend override must NOT be overridden by ask.rewriter_timeout_secs"
        );
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
        let ask = AskConfig {
            backend: Some(BackendConfig {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                endpoint: None,
                api_key_env: None,
                api_key_ref: None,
                timeout_secs: Some(42),
            }),
            ..Default::default()
        };
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
        let r = RollupConfig {
            abstractive_backend: Some(BackendConfig {
                provider: "ollama".into(),
                model: "qwen3:4b".into(),
                endpoint: Some("http://box.local:11434".into()),
                api_key_env: None,
                api_key_ref: None,
                timeout_secs: None,
            }),
            ..Default::default()
        };
        let b = r.effective_abstractive_backend(&omlx_llm());
        assert_eq!(b.provider, "ollama");
        assert_eq!(b.endpoint.as_deref(), Some("http://box.local:11434"));
    }
}
