//! LLM-backed session reflector with heuristic fallback.
//!
//! `reflect_session` checks the `ModelRegistry` for a "reflector" role. If
//! one is configured, it calls the designated model (Anthropic or Ollama) to
//! score injected patterns against the transcript. On any error — missing API
//! key, network failure, JSON parse error — it falls back to the pure-keyword
//! heuristic in `capture::feedback`.
//!
//! The module is part of the E2 library surface; `reflect_session` is not yet
//! called from the CLI binary, so dead_code is expected until E2.3 wires it in.

#![allow(dead_code)]

use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::secret::SecretRef;
use serde::Deserialize;

use crate::capture::feedback::{InjectedPatternRecord, SessionFeedback, SignalType};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Result of reflecting a single injected pattern against a session transcript.
#[derive(Debug, Clone)]
pub struct ReflectResult {
    pub pattern_name: String,
    pub signal: SignalType,
    pub evidence: Option<String>,
    pub confidence_delta: f32,
    /// `true` when the result came from an LLM call, `false` for heuristic.
    pub llm_used: bool,
}

// ─── Main entry point ─────────────────────────────────────────────────────────

/// Reflect a session transcript against a set of injected patterns.
///
/// Uses the model registered under the "reflector" role when available;
/// otherwise falls back to the keyword-matching heuristic in `feedback`.
pub fn reflect_session(
    transcript: &str,
    injected: &[InjectedPatternRecord],
    registry: &ModelRegistry,
) -> Vec<ReflectResult> {
    if injected.is_empty() {
        return Vec::new();
    }

    // Try LLM path if a "reflector" role is configured and resolvable.
    if let Some(model_id) = registry.resolve_role("reflector")
        && let Some(entry) = registry.models.get(model_id)
    {
        let pattern_names: Vec<String> = injected.iter().map(|r| r.name.clone()).collect();
        match call_llm(transcript, &pattern_names, entry) {
            Ok(results) if !results.is_empty() => return results,
            Ok(_) => {
                tracing::debug!("reflector: LLM returned empty result, using heuristic");
            }
            Err(e) => {
                tracing::debug!("reflector: LLM call failed ({e}), using heuristic");
            }
        }
    }

    heuristic_fallback(transcript, injected)
}

// ─── Heuristic fallback ───────────────────────────────────────────────────────

fn heuristic_fallback(transcript: &str, injected: &[InjectedPatternRecord]) -> Vec<ReflectResult> {
    crate::capture::feedback::analyze_session_feedback(transcript, injected)
        .into_iter()
        .map(|sf: SessionFeedback| ReflectResult {
            pattern_name: sf.pattern_name,
            signal: sf.signal,
            evidence: sf.evidence,
            confidence_delta: sf.confidence_delta as f32,
            llm_used: false,
        })
        .collect()
}

// ─── LLM call ─────────────────────────────────────────────────────────────────

/// Shape returned by the LLM in the JSON array.
#[derive(Debug, Deserialize)]
struct LlmItem {
    name: String,
    signal: String,
    #[serde(default)]
    evidence: Option<String>,
    #[serde(default)]
    confidence_delta: f32,
}

fn call_llm(
    transcript: &str,
    pattern_names: &[String],
    entry: &ModelEntry,
) -> anyhow::Result<Vec<ReflectResult>> {
    let system = "You are a pattern quality evaluator. \
        Analyze AI session transcripts and return JSON.";

    let snippet = if transcript.len() > 200 {
        &transcript[..200]
    } else {
        transcript
    };

    let names_list = pattern_names.join(", ");
    let user = format!(
        "Patterns injected: {names_list}.\n\
        Transcript excerpt: {snippet}\n\n\
        Return a JSON array (no prose, no markdown) with one object per pattern:\n\
        [{{\"name\":\"<pattern_name>\",\
        \"signal\":\"reinforced\"|\"contradicted\"|\"ignored\",\
        \"evidence\":\"<quote or null>\",\
        \"confidence_delta\":<float -0.2 to 0.2>}}]"
    );

    let raw = match entry.provider.as_str() {
        "ollama" => call_ollama(&entry.model, entry.base_url.as_deref(), system, &user)?,
        _ => call_anthropic(&entry.model, entry.secret.as_ref(), system, &user)?,
    };

    Ok(parse_llm_json(&raw, pattern_names))
}

fn call_anthropic(
    model: &str,
    secret: Option<&SecretRef>,
    system: &str,
    user: &str,
) -> anyhow::Result<String> {
    // Only SecretRef::Env is supported here; full resolution requires async.
    let api_key = if let Some(SecretRef::Env(var)) = secret {
        std::env::var(var).ok()
    } else {
        None
    }
    .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
    .ok_or_else(|| anyhow::anyhow!("no Anthropic API key available"))?;

    let base = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_string());

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "system": system,
        "messages": [{"role": "user", "content": user}]
    });

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        anyhow::bail!("Anthropic API error {status}: {text}");
    }

    let json: serde_json::Value = resp.json()?;
    json["content"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|item| item["text"].as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("unexpected Anthropic response shape"))
}

fn call_ollama(
    model: &str,
    base_url: Option<&str>,
    system: &str,
    user: &str,
) -> anyhow::Result<String> {
    let base = base_url.unwrap_or("http://localhost:11434");

    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user",   "content": user}
        ]
    });

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{base}/api/chat"))
        .header("content-type", "application/json")
        .json(&body)
        .send()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        anyhow::bail!("Ollama API error {status}: {text}");
    }

    let json: serde_json::Value = resp.json()?;
    json["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("unexpected Ollama response shape"))
}

// ─── JSON parser (pub(crate) for tests) ───────────────────────────────────────

/// Parse the LLM JSON array into `ReflectResult`s.
///
/// Returns an empty vec on any parse error; the caller then falls back to
/// the heuristic.
pub(crate) fn parse_llm_json(raw: &str, pattern_names: &[String]) -> Vec<ReflectResult> {
    // Strip markdown code fences if the model wrapped the JSON.
    let trimmed = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Try to find the JSON array even if there's surrounding prose.
    let json_start = trimmed.find('[').unwrap_or(0);
    let json_end = trimmed.rfind(']').map(|i| i + 1).unwrap_or(trimmed.len());
    let json_slice = &trimmed[json_start..json_end];

    let items: Vec<LlmItem> = match serde_json::from_str(json_slice) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    items
        .into_iter()
        .filter(|item| pattern_names.iter().any(|n| n == &item.name))
        .map(|item| ReflectResult {
            pattern_name: item.name,
            signal: parse_signal(&item.signal),
            evidence: item.evidence,
            confidence_delta: item.confidence_delta.clamp(-0.2, 0.2),
            llm_used: true,
        })
        .collect()
}

fn parse_signal(s: &str) -> SignalType {
    match s.to_lowercase().as_str() {
        "reinforced" => SignalType::Reinforced,
        "contradicted" => SignalType::Contradicted,
        _ => SignalType::Ignored,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::model::ModelRegistry;

    fn make_injected(name: &str) -> InjectedPatternRecord {
        InjectedPatternRecord {
            name: name.to_string(),
            snippet: "use async/await".to_string(),
        }
    }

    #[test]
    fn test_heuristic_fallback_no_role() {
        // Empty registry → no reflector role → heuristic path.
        let registry = ModelRegistry::default();
        let results = reflect_session(
            "some transcript with async/await usage",
            &[make_injected("rust-async")],
            &registry,
        );
        assert!(!results.is_empty());
        assert!(
            !results[0].llm_used,
            "should use heuristic when no role configured"
        );
    }

    #[test]
    fn test_parse_llm_json_response() {
        let json = r#"[{"name":"rust-async","signal":"reinforced","evidence":"used async/await","confidence_delta":0.05}]"#;
        let parsed = parse_llm_json(json, &["rust-async".to_string()]);
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0].signal, SignalType::Reinforced));
        assert!(parsed[0].llm_used);
        assert!((parsed[0].confidence_delta - 0.05).abs() < 1e-6);
    }

    #[test]
    fn test_parse_llm_json_invalid_falls_back_to_empty() {
        let parsed = parse_llm_json("not valid json", &["rust-async".to_string()]);
        assert!(
            parsed.is_empty(),
            "invalid JSON should produce empty vec for heuristic fallback"
        );
    }

    #[test]
    fn test_parse_llm_json_contradicted() {
        let json = r#"[{"name":"old-api","signal":"contradicted","evidence":"don't use this","confidence_delta":-0.15}]"#;
        let parsed = parse_llm_json(json, &["old-api".to_string()]);
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0].signal, SignalType::Contradicted));
        assert_eq!(parsed[0].evidence.as_deref(), Some("don't use this"));
    }

    #[test]
    fn test_parse_llm_json_ignored() {
        let json = r#"[{"name":"pat","signal":"ignored","confidence_delta":-0.01}]"#;
        let parsed = parse_llm_json(json, &["pat".to_string()]);
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0].signal, SignalType::Ignored));
    }

    #[test]
    fn test_parse_llm_json_strips_markdown_fences() {
        let json =
            "```json\n[{\"name\":\"p\",\"signal\":\"reinforced\",\"confidence_delta\":0.1}]\n```";
        let parsed = parse_llm_json(json, &["p".to_string()]);
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn test_parse_llm_json_unknown_pattern_filtered() {
        // Names not in the allow-list should be silently dropped.
        let json = r#"[{"name":"unknown","signal":"reinforced","confidence_delta":0.1}]"#;
        let parsed = parse_llm_json(json, &["known-pattern".to_string()]);
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_confidence_delta_clamped() {
        let json = r#"[{"name":"p","signal":"reinforced","confidence_delta":9.9}]"#;
        let parsed = parse_llm_json(json, &["p".to_string()]);
        assert_eq!(parsed.len(), 1);
        assert!(
            parsed[0].confidence_delta <= 0.2,
            "delta should be clamped to 0.2"
        );
    }

    #[test]
    fn test_empty_injected_returns_empty() {
        let registry = ModelRegistry::default();
        let results = reflect_session("some transcript", &[], &registry);
        assert!(results.is_empty());
    }

    #[test]
    fn test_heuristic_reinforced_signal() {
        let registry = ModelRegistry::default();
        let transcript = "The async/await pattern worked perfectly here.";
        let injected = vec![make_injected("rust-async")];
        let results = reflect_session(transcript, &injected, &registry);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].signal, SignalType::Reinforced));
        assert!(!results[0].llm_used);
    }
}
