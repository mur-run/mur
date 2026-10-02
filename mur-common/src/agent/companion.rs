use super::*;

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
    /// IGNORED since 2.79 (kept so old profiles load; warned at agent start).
    /// Bounds live in `limits:` — see `mur limits <agent>`.
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// IGNORED since 2.79 (kept so old profiles load; warned at agent start).
    /// Bounds live in `limits:` — see `mur limits <agent>`.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// How far this agent carries a turn before handing back (issue #001):
    /// `continue` / `review` / `ask`. `None` = inherit the built-in default,
    /// which is the strictest (`ask`) — turning an agent loose is a thing you
    /// write down, never a thing you get by leaving a key out.
    ///
    /// Lives here, beside `timeout_secs`, because it is human-in-the-loop
    /// vocabulary; it does NOT live in `limits:`, which is budgets. The two
    /// are enforced at different seams and must not be confusable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<crate::hitl::Autonomy>,
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
            autonomy: None,
        }
    }
}

#[cfg(test)]
mod hitl_tests {
    use super::*;

    /// #001: an agent profile that says nothing about autonomy inherits the
    /// strict default. Absent must never read as "turn it loose".
    #[test]
    fn hitl_config_autonomy_absent_means_inherit_not_continue() {
        let cfg: HitlConfig = serde_yaml::from_str("timeout_secs: 60").unwrap();
        assert_eq!(cfg.autonomy, None);
        assert_eq!(cfg.autonomy.unwrap_or_default(), crate::hitl::Autonomy::Ask);
    }

    #[test]
    fn hitl_config_autonomy_parses_all_three_modes() {
        for (yaml, want) in [
            ("continue", crate::hitl::Autonomy::Continue),
            ("review", crate::hitl::Autonomy::Review),
            ("ask", crate::hitl::Autonomy::Ask),
        ] {
            let cfg: HitlConfig =
                serde_yaml::from_str(&format!("timeout_secs: 60\nautonomy: {yaml}")).unwrap();
            assert_eq!(cfg.autonomy, Some(want), "yaml={yaml}");
        }
    }

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

/// Resolve a default BCP-47 locale: the OS locale first (sys-locale already
/// returns BCP-47, e.g. `zh-Hant-TW`), then the `LANG` environment variable
/// (POSIX form `zh_TW.UTF-8` → `zh-TW`), then `en-US`.
///
/// OS-first matters because this is also the serde default for
/// `AgentProfile.locale`: under launchd there is no `LANG`, so the old
/// LANG-only resolution silently defaulted every headless agent to `en-US`
/// even on a non-English system.
pub fn default_locale() -> String {
    sys_locale::get_locale()
        .filter(|l| !l.is_empty())
        .or_else(|| std::env::var("LANG").ok().and_then(|v| normalize_lang(&v)))
        .unwrap_or_else(|| "en-US".into())
}

/// Parse a POSIX-style `LANG` value into BCP-47 (`zh_TW.UTF-8` → `zh-TW`).
fn normalize_lang(v: &str) -> Option<String> {
    v.split('.')
        .next()
        .map(|s| s.replace('_', "-"))
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod locale_tests {
    use super::normalize_lang;

    #[test]
    fn lang_with_encoding_and_region_normalizes() {
        assert_eq!(normalize_lang("zh_TW.UTF-8").as_deref(), Some("zh-TW"));
    }

    #[test]
    fn lang_without_encoding_normalizes() {
        assert_eq!(normalize_lang("en_US").as_deref(), Some("en-US"));
    }

    #[test]
    fn lang_with_script_keeps_script() {
        assert_eq!(
            normalize_lang("zh_Hant_TW.UTF-8").as_deref(),
            Some("zh-Hant-TW")
        );
    }

    #[test]
    fn empty_lang_yields_none() {
        assert_eq!(normalize_lang(""), None);
    }
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
mod voice_tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn voice_config_round_trips() {
        // Base: use the canonical minimal fixture and append a voice: block.
        let base = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
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
        let base = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
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
        let yaml = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
        let profile: AgentProfile = serde_yaml_ng::from_str(yaml).expect("parse legacy");
        assert_eq!(profile.appearance.style_preset, "default-blob");
        assert_eq!(profile.appearance.behavior_preset, BehaviorPreset::Normal);
        assert_eq!(profile.appearance.render_status, RenderStatus::Pending);
    }

    #[test]
    fn legacy_profile_without_file_actions_or_action_pipeline_loads() {
        let yaml = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
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
