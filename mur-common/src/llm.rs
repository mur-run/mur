use crate::error::LlmError;

/// Trait for LLM providers (Anthropic, OpenAI, Ollama).
/// Shared between mur-core and mur-commander.
///
/// Edition 2024 supports async fn in traits natively.
pub trait LlmClient: Send + Sync {
    /// Text completion
    fn complete(
        &self,
        prompt: &str,
        system: Option<&str>,
    ) -> impl Future<Output = Result<String, LlmError>> + Send;

    /// Generate embedding vector
    fn embed(&self, text: &str) -> impl Future<Output = Result<Vec<f32>, LlmError>> + Send;
}

use std::future::Future;

/// Default Anthropic API base URL.
pub const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Resolve the Anthropic API base URL from `ANTHROPIC_BASE_URL` env, with a
/// trailing slash stripped. Falls back to `ANTHROPIC_DEFAULT_BASE_URL`.
///
/// Honored at every upstream call site so that users can route Anthropic
/// traffic through Bedrock, Vertex, a corporate egress proxy, an external
/// auth bridge, or test fixtures without touching code.
pub fn anthropic_base_url() -> String {
    let raw = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| ANTHROPIC_DEFAULT_BASE_URL.to_string());
    raw.trim_end_matches('/').to_string()
}

/// Check if a model name matches recommended reasoning models for session analysis.
///
/// Recommended: Anthropic Opus, OpenAI GPT-5/O3/O4, Gemini Pro 3+,
/// or any model with "reasoning" or "think" in the name.
#[allow(clippy::collapsible_if)]
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model.to_lowercase();

    if m.contains("opus") {
        return true;
    }
    if m.contains("gpt-5") || m.contains("o3") || m.contains("o4") {
        return true;
    }
    if m.contains("gemini") && m.contains("pro") {
        // The version may follow ("gemini-pro-3.5") or precede ("gemini-3.5-pro")
        // the tier, so take the major version from the first number in the name.
        if let Some(start) = m.find(|c: char| c.is_ascii_digit()) {
            let tail = &m[start..];
            let end = tail
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(tail.len());
            if let Ok(v) = tail[..end].parse::<u32>()
                && v >= 3
            {
                return true;
            }
        }
    }
    if m.contains("reasoning") || m.contains("think") {
        return true;
    }
    false
}

/// How hard the model should work on a request — the Anthropic
/// `output_config.effort` scale.
///
/// Effort is a property of the JOB, not of the model: the same model routing a
/// one-line JSON plan and the same model doing a multi-file refactor want
/// different levels. Set it at the call site that knows what it is asking for.
///
/// Note that NOT sending effort is not neutral — the API default is `High`.
/// Every call that leaves it unset is already paying for high effort, so the
/// useful direction for mechanical work is *down*.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// Every level, cheapest first — for `--help` text and error messages, so
    /// the valid set is never spelled out in two places.
    pub const ALL: &'static [Effort] = &[
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

impl std::str::FromStr for Effort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let want = s.trim().to_lowercase();
        Effort::ALL
            .iter()
            .copied()
            .find(|e| e.as_str() == want)
            .ok_or_else(|| {
                let valid: Vec<&str> = Effort::ALL.iter().map(|e| e.as_str()).collect();
                format!("unknown effort '{s}' (valid: {})", valid.join(", "))
            })
    }
}

/// What form of reasoning control a model accepts.
///
/// Provider controls do not share a shape, and three of these cannot be
/// expressed by a level-to-string table:
///
/// * [`EffortShape::AlwaysOn`] is not [`EffortShape::None`]. Mistral's
///   Magistral models always reason and reject `reasoning_effort` with HTTP
///   422 — a hard failure, not a degradation. `None` means "passing nothing is
///   correct"; `AlwaysOn` means "passing anything breaks every call".
/// * [`EffortShape::Binary`] is not a degenerate `Graded`. Qwen and GLM have a
///   switch, not a dial, and spell it differently (`chat_template_kwargs`
///   versus `thinking: {type}`). Two other agent products shipped that exact
///   confusion.
/// * [`EffortShape::Budget`] takes an integer on the wire but still carries a
///   level list, because the user and both UIs deal in levels — only the
///   client converts, at the last possible moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortShape {
    /// Levels this model accepts, cheapest first.
    Graded(&'static [Effort]),
    /// Thinking is a switch; `on_at` is the lowest level that turns it on.
    Binary { on_at: Effort },
    /// Wants an integer token budget; these are the levels to offer.
    Budget(&'static [Effort]),
    /// Always reasons and rejects the parameter. Send nothing.
    AlwaysOn,
    /// No reasoning control.
    None,
}

/// A level set is an arbitrary SUBSET of [`Effort::ALL`], not a prefix of it.
/// Two tiers below have holes: Anthropic's pre-4.7 lines have `max` but no
/// `xhigh` (that step was inserted between them in 4.7), and DeepSeek V4
/// publishes low/high/max with no medium. Naming these by a ceiling —
/// `LEVELS_TO_HIGH` and the like — is what produced a first draft that silently
/// downgraded `max` to `high` on Opus 4.6. Name the membership, not a bound.
const LEVELS_ALL: &[Effort] = &[
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];
/// Anthropic before 4.7: the whole scale except the `xhigh` step.
const LEVELS_NO_XHIGH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::Max];
const LEVELS_LMHX: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh];
const LEVELS_LMH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High];
/// DeepSeek V4 publishes low / high / max — there is no medium step.
const LEVELS_DEEPSEEK: &[Effort] = &[Effort::Low, Effort::High, Effort::Max];
/// A switch has two positions and must offer two, not three that collapse to
/// two. `Low` is off, `High` is on; the threshold lives in `Binary::on_at`.
const LEVELS_BINARY: &[Effort] = &[Effort::Low, Effort::High];

impl EffortShape {
    /// The levels a UI should offer. Empty for the two shapes that take no
    /// level from the user.
    pub fn levels(&self) -> &'static [Effort] {
        match self {
            EffortShape::Graded(l) | EffortShape::Budget(l) => l,
            EffortShape::Binary { .. } => LEVELS_BINARY,
            EffortShape::AlwaysOn | EffortShape::None => &[],
        }
    }
}

/// Which reasoning control `model` accepts.
///
/// Keyed on the model id with any `vendor/` prefix stripped, never on the
/// registry's `provider:` field — that records the wire protocol, so DeepSeek,
/// Qwen and every other OpenAI-compatible third party all read `openai`.
///
/// Capability is version-scoped, not family-scoped: grok-4.3, 4.5 and 4.6 each
/// accept a different set, exactly as the Claude 4-5 / 4-6 / 4-7 / 5 lines do.
/// Named const lists per tier, so a new model is one edit in one place.
///
/// Vendor levels below `Low` (`none`, `minimal`) are deliberately dropped:
/// MUR's scale starts at `Low`, and a level MUR cannot name is a level MUR
/// does not offer. Nothing is mis-sent.
pub fn effort_shape(model: &str) -> EffortShape {
    /// Anthropic lines with the `xhigh` step (Opus 4.7 and later).
    const ANTHROPIC_FULL: &[&str] = &[
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-mythos-5",
    ];
    /// Anthropic lines that take effort but have no `xhigh` step.
    const ANTHROPIC_NO_XHIGH: &[&str] =
        &["claude-opus-4-6", "claude-sonnet-4-6", "claude-opus-4-5"];
    /// OpenAI reasoning families. `xhigh`/`max` clamp to `high` at the wire.
    const OPENAI_REASONING: &[&str] = &["gpt-5", "o1", "o3", "o4"];
    /// DeepSeek V4 thinking effort.
    const DEEPSEEK_GRADED: &[&str] = &["deepseek-v4"];
    /// Grok lines that added the `xhigh` step.
    const GROK_XHIGH: &[&str] = &["grok-4.6", "grok-4.7"];
    /// Grok lines with low/medium/high only.
    const GROK_GRADED: &[&str] = &["grok-4.3", "grok-4.5"];
    /// Gemini 3+ takes `thinkingConfig.thinkingLevel` — a level name.
    const GEMINI_LEVEL: &[&str] = &["gemini-3"];
    /// Gemini 2.5 takes `thinkingConfig.thinkingBudget` — an integer.
    const GEMINI_BUDGET: &[&str] = &["gemini-2.5"];
    /// Mistral hybrids where `reasoning_effort` enables reasoning.
    const MISTRAL_GRADED: &[&str] = &["mistral-small-3", "mistral-small-4"];
    /// Dedicated reasoning models that REJECT the parameter with HTTP 422.
    const ALWAYS_ON: &[&str] = &["magistral"];
    /// Models whose only control is an on/off switch.
    const BINARY: &[&str] = &["qwen3", "glm-"];

    let m = model.to_lowercase();
    let bare = m.rsplit('/').next().unwrap_or(&m);
    let has = |prefixes: &[&str]| prefixes.iter().any(|p| bare.starts_with(p));

    // AlwaysOn is checked first: sending anything to these is a hard error, so
    // no later arm may claim them.
    if has(ALWAYS_ON) {
        return EffortShape::AlwaysOn;
    }
    if has(ANTHROPIC_FULL) {
        return EffortShape::Graded(LEVELS_ALL);
    }
    // Anthropic pre-4.7 keeps `max`; it is only the `xhigh` step it lacks.
    if has(ANTHROPIC_NO_XHIGH) {
        return EffortShape::Graded(LEVELS_NO_XHIGH);
    }
    if has(OPENAI_REASONING) || has(GROK_GRADED) {
        return EffortShape::Graded(LEVELS_LMH);
    }
    if has(GROK_XHIGH) {
        return EffortShape::Graded(LEVELS_LMHX);
    }
    if has(DEEPSEEK_GRADED) {
        return EffortShape::Graded(LEVELS_DEEPSEEK);
    }
    if has(GEMINI_LEVEL) || has(MISTRAL_GRADED) {
        return EffortShape::Graded(LEVELS_LMH);
    }
    if has(GEMINI_BUDGET) {
        return EffortShape::Budget(LEVELS_LMH);
    }
    if has(BINARY) {
        return EffortShape::Binary {
            on_at: Effort::Medium,
        };
    }
    EffortShape::None
}

/// Where the effort in force came from, so a surface can say so instead of
/// showing a bare level the user cannot account for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortSource {
    /// Set for this conversation only (murmur `/effort`).
    SessionOverride,
    /// Stored on the agent profile.
    Profile,
    /// Nothing set — the provider's own default applies. Note that is not
    /// "no effort": the API default is high.
    Unset,
}

/// The effort actually in force for `model`, and where it came from.
///
/// The ONLY derivation of this. murmur and the Hub both call it; neither
/// computes its own, because two surfaces answering one question differently
/// is a failure this codebase has already shipped twice.
///
/// A stored level the model cannot accept is narrowed to the nearest level it
/// can, and the source is preserved — the user did set it; they are simply
/// getting the closest thing that will not 400.
pub fn effective_effort(
    session: Option<Effort>,
    profile: Option<Effort>,
    model: &str,
) -> (Option<Effort>, EffortSource) {
    let (want, source) = match (session, profile) {
        (Some(e), _) => (e, EffortSource::SessionOverride),
        (None, Some(e)) => (e, EffortSource::Profile),
        (None, None) => return (None, EffortSource::Unset),
    };
    let levels = effort_shape(model).levels();
    if levels.is_empty() {
        return (None, EffortSource::Unset);
    }
    if levels.contains(&want) {
        return (Some(want), source);
    }
    match levels.iter().rev().find(|l| **l < want).copied() {
        Some(narrowed) => (Some(narrowed), source),
        // `want` is below every level this model offers; take its cheapest.
        None => (levels.first().copied(), source),
    }
}

/// The effort level to actually send for `model`, or `None` when the model
/// takes no effort parameter at all.
///
/// Sending an unsupported level is a 400, and the support matrix is per-model:
/// the `xhigh` step arrived with Opus 4.7, so older models that otherwise
/// accept effort reject it, and models before the 4.6 line reject the
/// parameter outright. Rather than let each call site memorise that, requests
/// state the effort they *want* and this narrows it — downgrading `xhigh` to
/// `high` (the cheaper neighbour) and dropping the field entirely where it
/// isn't understood.
///
/// Deliberately shaped like the sampling-param guard next door: one named
/// list, one place to edit when a model ships, rather than a literal model ID
/// buried in a conditional that goes stale on the next release.
pub fn supported_effort(model: &str, want: Effort) -> Option<Effort> {
    /// This mapper is Anthropic's. [`effort_shape`] is vendor-neutral — it
    /// answers "which levels", never "whose client is this" — so the vendor
    /// gate stays here, symmetric with `openai_reasoning_effort`'s family
    /// gate. Without it a delegated `supported_effort` starts claiming
    /// `gpt-5`, which an existing test caught immediately.
    const ANTHROPIC: &str = "claude-";

    let m = model.to_lowercase();
    let bare = m.rsplit('/').next().unwrap_or(&m);
    if !bare.starts_with(ANTHROPIC) {
        return None;
    }
    let levels = match effort_shape(model) {
        EffortShape::Graded(l) => l,
        // Anthropic is Graded or nothing. Budget/Binary models never reach this
        // client, and AlwaysOn must be sent nothing anywhere.
        _ => return None,
    };
    if levels.contains(&want) {
        return Some(want);
    }
    // Degrade to the most expensive level this model DOES accept rather than
    // send one it will 400 on. `levels` is a subset, not a prefix, so this
    // steps over holes (pre-4.7 Anthropic has `max` but no `xhigh`).
    levels.iter().rev().find(|l| **l < want).copied()
}

/// The `reasoning_effort` value to send for an OpenAI-compatible model, or
/// `None` when the model takes no such parameter.
///
/// Verified against the current OpenAI reasoning guide rather than recalled:
/// the parameter is `reasoning.effort` (accepted as `reasoning_effort` on the
/// chat endpoint) and its vocabulary is `none | minimal | low | medium | high
/// | xhigh | max` — a superset of ours, not the three levels an older memory
/// would suggest. Checking mattered: building the mapping from that memory
/// would have clamped `xhigh` away for no reason.
///
/// Two deliberate narrowings, both because this client is not "OpenAI" — it is
/// *anything OpenAI-compatible*, including OpenRouter and local servers:
///
/// * Gated on the model family, not the provider. A local llama behind an
///   OpenAI-shaped endpoint has no idea what `reasoning_effort` means.
/// * `Xhigh`/`Max` clamp to `high`. The docs state the accepted values are
///   model-dependent and publish no table, so passing the top of the scale
///   through would be a 400 waiting for whichever model lacks it. Degrading is
///   the same call made for Anthropic's missing `xhigh` step — and the same
///   one made everywhere today: never fail the default path, lose a little
///   depth instead. Lift the clamp per-model once a support table exists.
pub fn openai_reasoning_effort(model: &str, want: Effort) -> Option<&'static str> {
    /// Model families that take a reasoning effort. Prefixes, matched after
    /// any `vendor/` prefix is stripped — OpenRouter names models
    /// `openai/gpt-5`, and `google/gemini-3.6-flash` must NOT match.
    const REASONING_FAMILIES: &[&str] = &["gpt-5", "o1", "o3", "o4"];

    let m = model.to_lowercase();
    let bare = m.rsplit('/').next().unwrap_or(&m);
    if !REASONING_FAMILIES.iter().any(|f| bare.starts_with(f)) {
        return None;
    }
    // Second gate: the table is the owner, so a family member the table has
    // demoted (or that becomes AlwaysOn) stops being sent a value here too.
    if !matches!(effort_shape(model), EffortShape::Graded(_)) {
        return None;
    }
    Some(match want {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::Xhigh | Effort::Max => "high",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_effort_reports_value_and_where_it_came_from() {
        use EffortSource::*;
        // Session beats profile.
        assert_eq!(
            effective_effort(Some(Effort::Low), Some(Effort::Max), "claude-opus-5"),
            (Some(Effort::Low), SessionOverride)
        );
        // Profile when there is no session override.
        assert_eq!(
            effective_effort(None, Some(Effort::Max), "claude-opus-5"),
            (Some(Effort::Max), Profile)
        );
        // Neither set.
        assert_eq!(effective_effort(None, None, "claude-opus-5"), (None, Unset));
        // A value the model cannot take is narrowed, and the SOURCE is
        // preserved: the user still set it, they just get the nearest level
        // that works. DeepSeek V4 has no medium step.
        assert_eq!(
            effective_effort(None, Some(Effort::Medium), "deepseek-v4-pro"),
            (Some(Effort::Low), Profile)
        );
        // A model with no control reports nothing regardless of what is stored.
        assert_eq!(
            effective_effort(Some(Effort::Max), Some(Effort::Max), "gpt-4o"),
            (None, Unset)
        );
    }

    /// Delegation must not change what the mappers return. This pins the exact
    /// case a first draft of this plan got wrong: Anthropic's pre-4.7 lines keep
    /// `max` and lack only `xhigh`, so a level set expressed as a ceiling
    /// silently downgrades `max` to `high` here.
    #[test]
    fn delegation_preserves_the_hole_in_the_pre_4_7_scale() {
        // The hole: xhigh absent, max present.
        assert_eq!(
            supported_effort("claude-opus-4-6", Effort::Xhigh),
            Some(Effort::High)
        );
        assert_eq!(
            supported_effort("claude-opus-4-6", Effort::Max),
            Some(Effort::Max)
        );
        // The full scale is unaffected.
        assert_eq!(
            supported_effort("claude-opus-5", Effort::Xhigh),
            Some(Effort::Xhigh)
        );
        // A model the table calls AlwaysOn must get nothing from either mapper,
        // because sending it anything is a 422.
        assert_eq!(
            supported_effort("magistral-small-latest", Effort::High),
            None
        );
        assert_eq!(
            openai_reasoning_effort("magistral-small-latest", Effort::High),
            None
        );
    }

    #[test]
    fn effort_shape_covers_each_vendor_tier() {
        use EffortShape::*;
        assert!(matches!(effort_shape("claude-opus-5"), Graded(l) if l.len() == 5));
        // Four, not three: 4.6 keeps `max` and lacks only the `xhigh` step that
        // 4.7 inserted between them. A level set is a subset, not a prefix.
        assert!(matches!(effort_shape("claude-opus-4-6"), Graded(l) if l.len() == 4));
        assert!(matches!(effort_shape("claude-opus-4-6"), Graded(l) if l.contains(&Effort::Max)));
        assert!(
            matches!(effort_shape("claude-opus-4-6"), Graded(l) if !l.contains(&Effort::Xhigh))
        );
        assert!(matches!(effort_shape("gpt-5"), Graded(_)));
        assert!(matches!(effort_shape("grok-4.6"), Graded(l) if l.contains(&Effort::Xhigh)));
        assert!(matches!(effort_shape("grok-4.5"), Graded(l) if !l.contains(&Effort::Xhigh)));
        assert!(matches!(effort_shape("gemini-3-pro"), Graded(_)));
        assert!(matches!(effort_shape("gemini-2.5-pro"), Budget(_)));
        assert!(matches!(effort_shape("qwen3-32b"), Binary { .. }));
        // A switch offers exactly two positions, never three that collapse to two.
        assert_eq!(effort_shape("qwen3-32b").levels().len(), 2);
        assert!(matches!(effort_shape("glm-4.6"), Binary { .. }));
        assert!(matches!(effort_shape("magistral-medium-latest"), AlwaysOn));
        assert!(matches!(effort_shape("gpt-4o"), None));
        assert!(matches!(effort_shape("llama3.2:3b"), None));
    }

    /// The three cases the design turns on. Each must fail if its guard is removed.
    #[test]
    fn effort_shape_negative_cases() {
        use EffortShape::*;
        // 1. DeepSeek V4 has low/high/max and NO medium. Offering medium is a 400.
        let EffortShape::Graded(levels) = effort_shape("deepseek-v4-pro") else {
            panic!("deepseek-v4-pro must be Graded");
        };
        assert!(
            !levels.contains(&Effort::Medium),
            "deepseek has no medium: {levels:?}"
        );
        assert_eq!(levels, &[Effort::Low, Effort::High, Effort::Max]);

        // 2. A vendor prefix must be stripped before matching, and a Google model
        //    must never match an OpenAI family prefix.
        assert!(matches!(effort_shape("openai/gpt-5"), Graded(_)));
        assert!(matches!(effort_shape("google/gemini-3.6-flash"), Graded(_)));
        assert!(!matches!(
            effort_shape("google/gemini-2.5-flash"),
            Graded(_)
        ));

        // 3. Magistral REJECTS the parameter (HTTP 422). AlwaysOn is not None:
        //    None means "send nothing and that is correct", AlwaysOn means
        //    "send nothing or every call fails".
        assert!(matches!(effort_shape("magistral-small-latest"), AlwaysOn));
        assert!(effort_shape("magistral-small-latest").levels().is_empty());
    }

    #[test]
    fn supported_effort_narrows_per_model_capability() {
        // Full scale: passed through unchanged.
        assert_eq!(
            supported_effort("claude-opus-5", Effort::Xhigh),
            Some(Effort::Xhigh)
        );
        assert_eq!(
            supported_effort("claude-sonnet-5", Effort::Low),
            Some(Effort::Low)
        );
        // Prefix match, so dated or suffixed variants resolve the same.
        assert_eq!(
            supported_effort("claude-opus-5-preview", Effort::Max),
            Some(Effort::Max)
        );
        // No `xhigh` step before Opus 4.7 — downgrade to the cheaper neighbour
        // rather than 400.
        assert_eq!(
            supported_effort("claude-opus-4-6", Effort::Xhigh),
            Some(Effort::High)
        );
        // …but the levels it does have pass through.
        assert_eq!(
            supported_effort("claude-opus-4-6", Effort::Max),
            Some(Effort::Max)
        );
        // Models with no effort parameter: drop the field entirely.
        assert_eq!(supported_effort("claude-haiku-4-5", Effort::Low), None);
        assert_eq!(supported_effort("claude-sonnet-4-5", Effort::High), None);
        // Non-Anthropic models never carry it.
        assert_eq!(supported_effort("llama3.2:3b", Effort::Low), None);
        assert_eq!(supported_effort("gpt-5", Effort::Low), None);
    }

    #[test]
    fn effort_strings_match_the_api_scale() {
        assert_eq!(Effort::Low.as_str(), "low");
        assert_eq!(Effort::Xhigh.as_str(), "xhigh");
        assert_eq!(Effort::Max.as_str(), "max");
        // Ordered cheapest-first so a caller can clamp with `min`.
        assert!(Effort::Low < Effort::High && Effort::High < Effort::Max);
    }

    #[test]
    fn openai_effort_gates_on_family_and_clamps_the_top() {
        // The three shared levels pass through by name.
        assert_eq!(openai_reasoning_effort("gpt-5", Effort::Low), Some("low"));
        assert_eq!(
            openai_reasoning_effort("o3-mini", Effort::Medium),
            Some("medium")
        );
        // Top of the scale degrades rather than risking a 400 on a model whose
        // subset lacks it — the accepted values are model-dependent and there
        // is no published table to key on.
        assert_eq!(
            openai_reasoning_effort("gpt-5", Effort::Xhigh),
            Some("high")
        );
        assert_eq!(openai_reasoning_effort("gpt-5", Effort::Max), Some("high"));
        // OpenRouter prefixes its models; the family gate must see through it…
        assert_eq!(
            openai_reasoning_effort("openai/gpt-5", Effort::Low),
            Some("low")
        );
        // …without letting a non-OpenAI model routed the same way through.
        assert_eq!(
            openai_reasoning_effort("google/gemini-3.6-flash", Effort::Low),
            None
        );
        // A local model behind an OpenAI-shaped endpoint takes no such param.
        assert_eq!(openai_reasoning_effort("llama3.2:3b", Effort::Low), None);
        assert_eq!(openai_reasoning_effort("gpt-4o", Effort::Low), None);
    }

    #[test]
    fn test_is_reasoning_model() {
        // Anthropic opus models
        assert!(is_reasoning_model("claude-opus-5"));
        assert!(is_reasoning_model("claude-opus-4-20250514"));

        // OpenAI reasoning models
        assert!(is_reasoning_model("gpt-5"));
        assert!(is_reasoning_model("chatgpt-5.4"));
        assert!(is_reasoning_model("o3-mini"));
        assert!(is_reasoning_model("o4-preview"));

        // Gemini pro >= 3 (version before or after the tier)
        assert!(is_reasoning_model("gemini-pro-3.5"));
        assert!(is_reasoning_model("gemini-pro-3"));
        assert!(is_reasoning_model("gemini-3.5-pro"));
        assert!(!is_reasoning_model("gemini-2.5-pro"));
        assert!(!is_reasoning_model("gemini-pro-2"));
        assert!(!is_reasoning_model("gemini-pro-1.5"));

        // Generic reasoning/thinking
        assert!(is_reasoning_model("deepseek-reasoning-v2"));
        assert!(is_reasoning_model("qwen-thinking-32b"));

        // Non-recommended
        assert!(!is_reasoning_model("claude-sonnet-4-20250514"));
        assert!(!is_reasoning_model("gpt-4o"));
        assert!(!is_reasoning_model("gemini-flash-2"));
        assert!(!is_reasoning_model("llama3"));
    }
}
