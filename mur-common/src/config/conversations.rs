use super::*;

// ── Ask config (Phase 2B, Task 18) ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskConfig {
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
    /// None = inherit the smart slot (`config.llm`).
    #[serde(default)]
    pub backend: Option<BackendConfig>,
    /// Per-stage backend override for the query rewriter.
    /// None = inherit the answer stage's backend (`self.backend`), falling
    /// through to the smart slot (`config.llm`) only if that is also unset.
    #[serde(default)]
    pub rewriter_backend: Option<BackendConfig>,
}

impl AskConfig {
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

    /// Effective backend for the query rewriter. An explicit
    /// `rewriter_backend` override wins outright. Otherwise the rewriter
    /// follows the answer stage's backend (`self.backend`), and only falls
    /// through to the smart slot (`llm`) when the answer stage has no
    /// override either — the rewriter is not an independent pinning point,
    /// only an independent timeout. Whichever source it resolves from, it
    /// always keeps its own much tighter `rewriter_timeout_secs` budget:
    /// the rewriter's output is small and falling back to the raw question
    /// on timeout is non-fatal.
    pub fn effective_rewriter_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.rewriter_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(self.rewriter_timeout_secs as u64),
                ..self
                    .backend
                    .clone()
                    .unwrap_or_else(|| llm.to_backend_config())
            })
    }
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
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
    /// None = inherit the smart slot (`config.llm`).
    #[serde(default)]
    pub extractive_backend: Option<BackendConfig>,
    /// Per-stage backend override for abstractive summarization.
    /// None = inherit the smart slot (`config.llm`).
    #[serde(default)]
    pub abstractive_backend: Option<BackendConfig>,
}

impl CompactConfig {
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
    extractive_backend:
      provider: anthropic
      model: claude-haiku-4-5
"#;
        let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
        let conv: ConversationsConfig = serde_yaml::from_value(v["conversations"].clone()).unwrap();
        assert_eq!(conv.compact.max_days_per_run, 3);
        assert_eq!(
            conv.compact.extractive_backend.as_ref().unwrap().model,
            "claude-haiku-4-5"
        );
        assert!(conv.compact.enabled_in_daemon); // default preserved
        assert!(conv.compact.abstractive_backend.is_none()); // default preserved
    }

    #[test]
    fn ask_config_defaults() {
        let c = AskConfig::default();
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
    min_score: 0.4
"#;
        let v: serde_yaml::Value = serde_yaml::from_str(y).unwrap();
        let conv: ConversationsConfig = serde_yaml::from_value(v["conversations"].clone()).unwrap();
        assert!(conv.ask.summarize_hits_enabled);
        assert!(conv.ask.summarize_model.is_none());
    }
}
