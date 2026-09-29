use super::*;

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
    // ── Per-kind decay curves (memory federation P1) ─────────────────────
    /// Half-life multiplier for `kind=rule` notes — behavioral guidance
    /// iterates fast, so it decays fast.
    pub note_rule_half_life_factor: f64,
    /// Half-life multiplier for `kind=fact` notes — environment truths
    /// decay slowly.
    pub note_fact_half_life_factor: f64,
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
            note_rule_half_life_factor: crate::skill::lifecycle::NOTE_RULE_HALF_LIFE_FACTOR,
            note_fact_half_life_factor: crate::skill::lifecycle::NOTE_FACT_HALF_LIFE_FACTOR,
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
