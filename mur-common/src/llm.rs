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
    /// Accept every level including `xhigh` (Opus 4.7 and later lines).
    const FULL_SCALE: &[&str] = &[
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-mythos-5",
    ];
    /// Accept effort but have no `xhigh` step.
    const NO_XHIGH: &[&str] = &["claude-opus-4-6", "claude-sonnet-4-6", "claude-opus-4-5"];

    let m = model.to_lowercase();
    if FULL_SCALE.iter().any(|p| m.starts_with(p)) {
        return Some(want);
    }
    if NO_XHIGH.iter().any(|p| m.starts_with(p)) {
        return Some(match want {
            Effort::Xhigh => Effort::High,
            other => other,
        });
    }
    // Everything else — older Claude models, and any non-Anthropic model that
    // reaches this path — takes no effort parameter.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
