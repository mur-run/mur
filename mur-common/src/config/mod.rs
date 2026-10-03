use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod attribution;
mod backend;
mod conversations;
mod ops;
mod scratch;
mod search;
mod skills;

pub use attribution::*;
pub use backend::*;
pub use conversations::*;
pub use ops::*;
pub use scratch::*;
pub use search::*;
pub use skills::*;

use ops::{
    default_heartbeat_interval_secs, default_heartbeat_stale_after_intervals, default_rotate_at_mb,
};

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

/// Smart background routing is opt-in. Its failure mode is silent and
/// irreversible for the turn it degrades, which is the kind of automation that
/// has to be asked for. See the capability-gate spec §2.
pub const DEFAULT_SMART_ENABLED: bool = false;

/// The Ollama provider default endpoint. Single definition — `BackendConfig`
/// resolution (`default_ollama_endpoint`), `config_migrate`, and the
/// conversations `doctor`/`preflight` probes all read this constant instead of
/// repeating the literal.
pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://localhost:11434";

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
fn default_smart_enabled() -> bool {
    DEFAULT_SMART_ENABLED
}

/// Smart background routing: auto-pick a cheap model for low-stakes/background
/// requests instead of always dialing the agent's primary model_ref. Defaults
/// OFF — enable it globally (`mur model smart on`) or per agent. `cheap: None`
/// auto-picks the cheapest registry entry that can serve the request
/// (`mur_common::model::pick_cheap_model`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmartConfig {
    #[serde(default = "default_smart_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,
    #[serde(default = "default_smart_max_escalations")]
    pub max_escalations: u32,
}

impl Default for SmartConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_SMART_ENABLED,
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
}

/// Partial view of [`SmartConfig`] for per-agent overrides: `None` on a field
/// means "inherit the global value". A distinct type rather than reusing
/// `SmartConfig`, because a full config standing in for a partial is exactly
/// what made an omitted field silently mean `false` instead of "unset".
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SmartOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_escalations: Option<u32>,
}

/// Partial view of [`RoutingConfig`]. Same inheritance rule as
/// [`SmartOverride`].
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RoutingOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_input_tokens: Option<u32>,
    /// Legacy nesting. Smart used to be overridden at `routing.smart`;
    /// profiles written before the promotion to `AgentProfile.smart` — and
    /// every exported `.muragent` bundle — still carry it, so it stays
    /// readable forever. MUR never writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart: Option<SmartOverride>,
}

impl SmartConfig {
    /// Global values with an agent's override layered on, field by field.
    pub fn merged(&self, ov: Option<&SmartOverride>) -> SmartConfig {
        let Some(o) = ov else { return self.clone() };
        SmartConfig {
            enabled: o.enabled.unwrap_or(self.enabled),
            cheap: o.cheap.clone().or_else(|| self.cheap.clone()),
            max_escalations: o.max_escalations.unwrap_or(self.max_escalations),
        }
    }
}

impl RoutingConfig {
    /// Global values with an agent's override layered on, field by field.
    pub fn merged(&self, ov: Option<&RoutingOverride>) -> RoutingConfig {
        let Some(o) = ov else { return self.clone() };
        RoutingConfig {
            enabled: o.enabled.unwrap_or(self.enabled),
            cheap: o.cheap.clone().or_else(|| self.cheap.clone()),
            frontier: o.frontier.clone().or_else(|| self.frontier.clone()),
            threshold_input_tokens: o.threshold_input_tokens.or(self.threshold_input_tokens),
        }
    }
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

    /// Memory-federation snapshot settings (`federation_snapshot:`).
    #[serde(default)]
    pub federation_snapshot: SnapshotConfig,

    /// Proactive memory capture (`memory:`, federation P2).
    #[serde(default)]
    pub memory: MemoryConfig,

    // --- fleet_run runtime built-in tool ---
    #[serde(default)]
    pub fleet_run: FleetRunConfig,

    // --- `mur open` display policy ---
    #[serde(default)]
    pub open_items: OpenItemsConfig,

    // --- Hub Fleet Manager redesign ---
    #[serde(default)]
    pub fleet: FleetConfig,

    /// `mur update` post-upgrade behavior (`update:`, issue #866).
    #[serde(default)]
    pub update: UpdateConfig,

    /// Job/fleet/workflow run-status heartbeat tuning (`runs:`).
    #[serde(default)]
    pub runs: RunsConfig,

    /// Capture-queue rotation (`capture:`).
    #[serde(default)]
    pub capture: CaptureConfig,

    /// Global execution limits (`limits:`), the outermost scope of spec
    /// 2026-09-12 §3.1. Written textually by `mur limits --global`, never by
    /// load-modify-save (that drops blocks other binaries own).
    #[serde(default)]
    pub limits: crate::limits::Limits,

    /// Durable-monitor notification channels (`notifications:`). Absent
    /// means log-only: `desktop` is opt-in.
    #[serde(default)]
    pub notifications: NotificationsConfig,

    /// AgentResolver (`monitor_resolver:`), spec §混合處置策略 step 3.
    /// Absent means off — see [`MonitorResolverConfig`].
    #[serde(default)]
    pub monitor_resolver: MonitorResolverConfig,

    /// Pre-dispatch triage (`triage:`). Absent means shadow mode: triage
    /// runs and records, and never stops a dispatch.
    #[serde(default)]
    pub triage: TriageConfig,

    /// MUR's credit line on work an agent publishes (`attribution:`).
    #[serde(default)]
    pub attribution: AttributionConfig,

    /// Per-agent scratch dir retention and doctor threshold (`scratch:`).
    #[serde(default)]
    pub scratch: ScratchConfig,

    /// Code-search tool bounds (`search:`), e.g. `search.ast_grep`.
    #[serde(default)]
    pub search: SearchConfig,
}

impl Config {
    /// Read from disk, falling back to defaults. Legacy conversation model
    /// fields are migrated **in memory only** — this is called from agent
    /// runtime processes (`mur-agent-runtime`), which must never write the
    /// user's config file.
    pub fn load_or_default(path: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let text = crate::config_migrate::migrate_conversations_yaml(&text).unwrap_or(text);
        let mut cfg: Self = serde_yaml_ng::from_str(&text).unwrap_or_default();
        cfg.sanitize();
        cfg
    }

    /// Clamp values that are legal YAML but illegal at runtime. Called once
    /// from [`Config::load_or_default`] — never from call sites, so every
    /// reader (CLI, executor heartbeat ticker, `status_of`'s stale
    /// threshold) sees the sanitized value.
    fn sanitize(&mut self) {
        // A zero interval would make the stale threshold zero (a healthy
        // run instantly reports STALLED) and `tokio::time::interval` panics
        // on a zero period — both from one user-edited line.
        if self.runs.heartbeat_interval_secs == 0 {
            self.runs.heartbeat_interval_secs = default_heartbeat_interval_secs();
        }
        // A zero rotate size would rotate on every single append.
        if self.capture.rotate_at_mb == 0 {
            self.capture.rotate_at_mb = default_rotate_at_mb();
        }
        if self.runs.heartbeat_stale_after_intervals == 0 {
            self.runs.heartbeat_stale_after_intervals = default_heartbeat_stale_after_intervals();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn update_defaults_apply_when_block_absent_and_fields_parse() {
        let c: super::Config = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(c.update, super::UpdateConfig::default());
        assert!(c.update.codesign_identity.is_none());
        assert!(c.update.restart_exclude.is_empty());

        let c: super::Config = serde_yaml_ng::from_str(
            "update:\n  codesign_identity: \"Developer ID Application: X (TEAM)\"\n  restart_exclude: [dr_worker_1]\n",
        )
        .unwrap();
        assert_eq!(
            c.update.codesign_identity.as_deref(),
            Some("Developer ID Application: X (TEAM)")
        );
        assert_eq!(c.update.restart_exclude, vec!["dr_worker_1".to_string()]);
    }

    #[test]
    fn federation_snapshot_defaults_apply_when_block_absent() {
        let c: super::Config = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(c.federation_snapshot.poll_secs, 30);
        assert_eq!(c.federation_snapshot.request_max_age_secs, 600);
        assert_eq!(
            c.federation_snapshot.min_lifecycle,
            crate::skill::stats::LifecycleState::Stable
        );
        // Memory capture (P2a): defaults to auto_announce; `off` parses.
        assert_eq!(c.memory.capture, super::CaptureMode::AutoAnnounce);
        let c: super::Config = serde_yaml_ng::from_str("memory:\n  capture: off\n").unwrap();
        assert_eq!(c.memory.capture, super::CaptureMode::Off);
    }

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
        let l2 = LlmConfig {
            api_key_ref: Some("keychain:mur/anthropic".into()),
            ..Default::default()
        };
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

    #[test]
    fn legacy_conversation_fields_are_gone_from_serialized_output() {
        let cfg = Config::default();
        // Scoped to the conversations block intentionally: `embedding` still
        // carries its own `ollama_endpoint`, and asserting over the whole
        // document here would require that field to be omitted, which is not
        // the case. The Config type will never serialize without it, so this
        // scoping to conversations is permanent.
        let yaml = serde_yaml_ng::to_string(&cfg.conversations).expect("serializes");
        for key in ["extractive_model", "abstractive_model", "ollama_endpoint"] {
            assert!(
                !yaml.contains(key),
                "legacy key {key} still serialized:\n{yaml}"
            );
        }
    }

    #[test]
    fn embedding_ollama_endpoint_is_omitted_when_unset() {
        let mut cfg = Config::default();
        cfg.embedding.provider = "omlx".into();
        cfg.embedding.openai_url = Some("http://127.0.0.1:8000/v1".into());
        cfg.embedding.ollama_endpoint = None;
        let yaml = serde_yaml_ng::to_string(&cfg).expect("serializes");
        assert!(
            !yaml.contains("ollama_endpoint"),
            "dead field re-emitted:\n{yaml}"
        );
    }

    #[test]
    fn embedding_ollama_endpoint_still_round_trips_when_set() {
        let yaml =
            "provider: ollama\nmodel: nomic-embed-text\nollama_endpoint: http://box.local:11434\n";
        let e: EmbeddingConfig = serde_yaml_ng::from_str(yaml).expect("parses");
        assert_eq!(e.ollama_endpoint.as_deref(), Some("http://box.local:11434"));
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
    fn smart_config_defaults_off_with_autopick() {
        let cfg: Config = serde_yaml::from_str("{}").unwrap();
        assert!(
            !cfg.models.smart.enabled,
            "Smart background routing is opt-in (capability-gate spec §2)"
        );
        assert_eq!(cfg.models.smart.cheap, None); // auto-pick
        assert_eq!(
            cfg.models.smart.max_escalations,
            DEFAULT_SMART_MAX_ESCALATIONS
        );
    }

    #[test]
    fn smart_override_inherits_field_by_field() {
        let global = SmartConfig {
            enabled: true,
            cheap: Some("g".into()),
            max_escalations: 3,
        };
        assert_eq!(global.merged(None), global);
        assert_eq!(global.merged(Some(&SmartOverride::default())), global);
        // Overriding one field must not reset the others — the whole point.
        let only_cheap = SmartOverride {
            cheap: Some("a".into()),
            ..Default::default()
        };
        let m = global.merged(Some(&only_cheap));
        assert!(m.enabled, "overriding cheap must not disable Smart");
        assert_eq!(m.cheap.as_deref(), Some("a"));
        assert_eq!(m.max_escalations, 3);
        // An explicit false still beats a global true.
        let off = SmartOverride {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!global.merged(Some(&off)).enabled);
    }

    #[test]
    fn routing_override_inherits_field_by_field() {
        let global = RoutingConfig {
            enabled: true,
            cheap: Some("c".into()),
            frontier: Some("f".into()),
            threshold_input_tokens: Some(9),
        };
        let only_cheap = RoutingOverride {
            cheap: Some("a".into()),
            ..Default::default()
        };
        let m = global.merged(Some(&only_cheap));
        assert!(m.enabled, "overriding cheap must not disable routing");
        assert_eq!(m.cheap.as_deref(), Some("a"));
        assert_eq!(m.frontier.as_deref(), Some("f"));
        assert_eq!(m.threshold_input_tokens, Some(9));
    }
}
