use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;

pub(crate) const DEFAULT_EXTRACT_PROMPT: &str = r#"You are MUR, a pattern extraction engine. Analyze the given AI assistant session transcript and extract reusable, generalizable patterns.

Extract patterns that represent:
1. Coding techniques or best practices demonstrated
2. Problem-solving approaches used
3. Tools or APIs used effectively
4. Common mistakes avoided or corrected

For each pattern output a JSON object with these fields:
- name: kebab-case identifier (e.g. "rust-error-handling")
- description: one-line summary
- technical: detailed technical explanation
- principle: (optional) underlying principle or insight
- tags: array of relevant tags
- tier: "session", "project", or "core"
- importance: 0.0 to 1.0

Return a JSON array of pattern objects. Return [] if no patterns are found."#;

#[allow(dead_code)]
pub(crate) fn parse_llm_patterns(response: &str) -> Vec<Pattern> {
    tracing::debug!(
        "LLM response (first 500 chars): {}",
        &response[..response.len().min(500)]
    );

    // Strip markdown fences if present
    let json_str = response
        .trim()
        .strip_prefix("```json")
        .or_else(|| response.trim().strip_prefix("```"))
        .unwrap_or(response.trim());
    let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

    #[derive(serde::Deserialize)]
    struct LlmPattern {
        name: String,
        description: String,
        technical: String,
        #[serde(default)]
        principle: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default = "default_tier_str")]
        tier: String,
        #[serde(default = "default_importance")]
        importance: f64,
    }
    fn default_tier_str() -> String {
        "session".to_string()
    }
    fn default_importance() -> f64 {
        0.5
    }

    // Handle explicit "no patterns" responses before JSON parsing
    // LLMs sometimes return `0`, `[]`, `null`, or `"none"` instead of an empty array
    let trimmed_lower = json_str.trim().to_lowercase();
    if matches!(
        trimmed_lower.as_str(),
        "0" | "[]" | "null" | "none" | "\"none\"" | "no patterns" | "n/a"
    ) {
        tracing::info!("LLM explicitly indicated no patterns to extract");
        return vec![];
    }

    // Try direct parse first, then fall back to extracting JSON array from text
    let items: Vec<LlmPattern> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => {
            // Fallback: find the outermost [...] in the response
            if let Some(start) = json_str.find('[')
                && let Some(end) = json_str.rfind(']')
                && start < end
            {
                let extracted = &json_str[start..=end];
                match serde_json::from_str::<Vec<LlmPattern>>(extracted) {
                    Ok(v) => v,
                    Err(e) => {
                        // Check if array contains non-object items (e.g. [0], [null])
                        // which means the LLM returned a "no patterns" signal
                        if let Ok(raw) = serde_json::from_str::<Vec<serde_json::Value>>(extracted)
                            && raw.iter().all(|v| !v.is_object())
                        {
                            tracing::info!(
                                "LLM returned non-pattern array (likely no patterns): {extracted}"
                            );
                            return vec![];
                        }
                        tracing::warn!("Failed to parse LLM pattern JSON: {e}");
                        tracing::debug!(
                            "Attempted to parse: {}",
                            &extracted[..extracted.len().min(300)]
                        );
                        return vec![];
                    }
                }
            } else if json_str.is_empty() {
                tracing::warn!("LLM returned an empty response");
                return vec![];
            } else {
                // LLM returned natural language instead of JSON
                tracing::info!("LLM returned non-JSON response (likely no patterns to extract)");
                tracing::debug!("Response: {}", &json_str[..json_str.len().min(200)]);
                return vec![];
            }
        }
    };

    let now = chrono::Utc::now();
    items
        .into_iter()
        .filter(|p| !p.name.is_empty() && !p.technical.is_empty())
        .map(|p| {
            let tier = match p.tier.as_str() {
                "core" => Tier::Core,
                "project" => Tier::Project,
                _ => Tier::Session,
            };
            Pattern {
                base: KnowledgeBase {
                    name: p.name,
                    description: p.description,
                    content: Content::DualLayer {
                        technical: p.technical,
                        principle: p.principle,
                    },
                    tier,
                    importance: p.importance.clamp(0.0, 1.0),
                    confidence: 0.6,
                    tags: Tags {
                        topics: p.tags,
                        ..Default::default()
                    },
                    maturity: mur_common::knowledge::Maturity::Draft,
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                },
                kind: None,
                #[allow(deprecated)] // transitional: user/platform fields being phased out
                origin: Some(Origin {
                    source: "llm-extract".to_string(),
                    trigger: OriginTrigger::Automatic,
                    actor: None,
                    user: None,
                    platform: None,
                    confidence: 0.6,
                }),
                attachments: vec![],
            }
        })
        .collect()
}
