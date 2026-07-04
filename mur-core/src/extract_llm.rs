//! LLM-enhanced workflow extraction.
//!
//! Builds on the pure-logic [`extract_workflow`] by sending the session
//! transcript plus the logic skeleton to an LLM (Haiku by default) for
//! richer names, descriptions, steps, variables, and trigger detection.
//! Results are cached to `~/.mur/session/recordings/{id}.extracted.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::Content;
use mur_common::workflow::{Step, VarType, Variable, Workflow};

use crate::extract::{ExtractedWorkflow, extract_workflow};
use crate::session::SessionEvent;

/// Cached LLM extraction result — serialized to JSON on disk.
#[derive(Debug, Serialize, Deserialize)]
struct LlmExtractedJson {
    name: String,
    description: String,
    steps: Vec<serde_json::Value>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    variables: serde_json::Value,
    #[serde(default)]
    trigger: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LlmVariable {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    default_value: Option<String>,
}

impl LlmExtractedJson {
    /// Normalize steps from various formats into plain strings.
    fn step_strings(&self) -> Vec<String> {
        self.steps
            .iter()
            .map(|s| {
                match s {
                    serde_json::Value::String(text) => text.clone(),
                    serde_json::Value::Object(obj) => {
                        // Try common field names: description, name, then action
                        // Skip generic actions like "execute_command"
                        obj.get("description")
                            .or(obj.get("name"))
                            .or(obj
                                .get("action")
                                .filter(|v| v.as_str().is_none_or(|s| !s.contains("execute"))))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                    }
                    _ => String::new(),
                }
            })
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Normalize variables from object-map or array format into LlmVariable vec.
    fn variable_list(&self) -> Vec<LlmVariable> {
        match &self.variables {
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| {
                    let name = v.get("name")?.as_str()?.to_string();
                    let description = v
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    let default_value = v
                        .get("default_value")
                        .or(v.get("default"))
                        .or(v.get("example"))
                        .map(|d| match d {
                            serde_json::Value::String(s) => s.clone(),
                            _ => d.to_string(),
                        });
                    Some(LlmVariable {
                        name,
                        description,
                        default_value,
                    })
                })
                .collect(),
            serde_json::Value::Object(map) => map
                .iter()
                .map(|(name, val)| {
                    let description = val
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    let default_value = val
                        .get("default_value")
                        .or(val.get("default"))
                        .or(val.get("example"))
                        .map(|d| match d {
                            serde_json::Value::String(s) => s.clone(),
                            _ => d.to_string(),
                        });
                    LlmVariable {
                        name: name.clone(),
                        description,
                        default_value,
                    }
                })
                .collect(),
            _ => vec![],
        }
    }
}

/// Extract a workflow using LLM enhancement with disk cache.
///
/// 1. Check cache (`{id}.extracted.json`) — return immediately if present.
/// 2. Run pure-logic `extract_workflow()` for the skeleton.
/// 3. Build a transcript from the session events (truncated to 4000 chars).
/// 4. Call the LLM (Haiku override) with skeleton + transcript.
/// 5. Parse JSON response; fallback to logic result on parse failure.
/// 6. Cache the result to disk.
pub async fn extract_workflow_llm(
    session_id: &str,
    events: &[SessionEvent],
) -> Result<ExtractedWorkflow> {
    // ── Check cache ──────────────────────────────────────────────────
    let cache_path = cache_path_for(session_id);
    if cache_path.exists()
        && let Ok(cached) = load_cached(&cache_path)
    {
        return Ok(build_workflow_from_llm(session_id, events, &cached));
    }

    // ── Logic skeleton ───────────────────────────────────────────────
    let logic_result = extract_workflow(session_id, events);

    // ── Load LLM config ──────────────────────────────────────────────
    let config =
        crate::store::config::load_config().context("Failed to load config for LLM extraction")?;
    let llm_config = config.llm.clone();

    // Use the model from config.yaml — user's choice (sonnet, opus, haiku, etc.)

    // ── Build transcript ─────────────────────────────────────────────
    let transcript = build_transcript(events);

    // ── Build skeleton summary ───────────────────────────────────────
    let skeleton = build_skeleton_summary(&logic_result);

    // ── LLM call ─────────────────────────────────────────────────────
    let system_prompt = r#"You are a workflow extraction assistant. Your ONLY job is to convert a session transcript into a reusable workflow template.

Do NOT analyze whether the session succeeded or failed. Do NOT report errors or status. Extract the INTENDED workflow pattern.

## Your Task

1. Read the user's original intent from the transcript
2. Extract a generalized, reusable workflow with parameterized variables
3. Return ONLY a JSON workflow definition

## Variable Extraction

Extract parameterizable values so the workflow is reusable with different inputs:
- `target_site` — website or service name (e.g., "博客來", "PChome", "Amazon")
- `search_term` — what to search for (e.g., "Rust", "AirPods Pro", "Python books")
- `count` — quantity or limit (e.g., 10, 5)
- `url` — any URL
- `file_path` — any file or directory path

Works with ANY language. Examples:
- "去博客來找最新的Python AI的10本書" → target_site=博客來, search_term=Python AI, count=10
- "find AirPods Pro prices on PChome 24h" → search_term=AirPods Pro, target_site=PChome 24h
- "search Amazon for top 5 wireless keyboards" → target_site=Amazon, search_term=wireless keyboards, count=5

## Output Format

Return ONLY this JSON structure (no markdown fences, no explanation, no analysis):
{
  "name": "short-kebab-case-name",
  "description": "One-sentence description of what this workflow does",
  "steps": ["Step 1 description", "Step 2 description"],
  "tools": ["Bash", "agent-browser"],
  "variables": [{"name": "var_name", "description": "what it is", "default_value": "the value from this session"}],
  "trigger": "when/how to trigger this workflow"
}"#;

    let user_prompt = format!(
        r#"Convert this session into a reusable workflow template.

The user's original request was the INTENT. The tool calls show the STEPS taken. Extract variables from the intent and generalize the steps.

## Logic-Extracted Skeleton
{skeleton}

## Raw Session Transcript
{transcript}

Now output the JSON workflow definition. Remember:
- Extract variables from the user's intent (target_site, search_term, count, etc.)
- Do NOT analyze errors or execution status — just extract the workflow pattern
- Output ONLY the JSON object, nothing else"#
    );

    // Build the cloud-LLM backend (RetryingBackend wraps it via factory::build_for_stage,
    // which dispatches on typed BackendError → {Timeout, ServerError(5xx), RateLimited}.
    // The previous hand-rolled 3-attempt loop classifying on 529/overload/timeout/503
    // substrings has been deleted in favor of that typed dispatch (P4 task 6).
    let backend_cfg = llm_config.to_backend_config();
    let backend = match crate::conversations::backend::factory::build_for_stage(
        &backend_cfg,
        "extract_llm",
    ) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("LLM backend init failed: {e:#}, falling back to logic extraction");
            return Ok(logic_result);
        }
    };
    let req = crate::conversations::backend::ChatRequest {
        model: &backend_cfg.model,
        system: Some(system_prompt),
        user: &user_prompt,
        max_tokens: 0, // backend default
        temperature: None,
        stop: vec![],
        cache_system: false,
        cache_user_prefix: None,
    };
    match backend.generate(req).await {
        Ok(resp) => match parse_llm_response(&resp.text) {
            Some(parsed) => {
                let _ = save_cache(&cache_path, &parsed);
                Ok(build_workflow_from_llm(session_id, events, &parsed))
            }
            None => {
                tracing::warn!("LLM returned invalid JSON, falling back to logic extraction");
                tracing::debug!(
                    "LLM response (first 2000 chars): {}",
                    &resp.text[..resp.text.len().min(2000)]
                );
                let _ = std::fs::write("/tmp/mur-llm-response.txt", &resp.text);
                Ok(logic_result)
            }
        },
        Err(e) => {
            tracing::warn!(
                "LLM call failed (after backend retries): {e:#}, falling back to logic extraction"
            );
            Ok(logic_result)
        }
    }
}

/// Check if an LLM config is usable (has a provider and the API key env is resolvable).
pub fn has_llm_config() -> bool {
    let config = match crate::store::config::load_config() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let llm = &config.llm;
    // Check the API key env var is set (unless Ollama which doesn't need one)
    if llm.provider == "ollama" {
        return true;
    }
    if let Some(r) = llm.api_key_ref.as_deref() {
        return r
            .parse::<mur_common::secret::SecretRef>()
            .map(|s| s.resolve_to_string_blocking().is_some())
            .unwrap_or(false);
    }
    let env_var = llm
        .api_key_env
        .as_deref()
        .unwrap_or(match llm.provider.as_str() {
            "anthropic" => "ANTHROPIC_API_KEY",
            "openai" => "OPENAI_API_KEY",
            "gemini" => "GEMINI_API_KEY",
            "openrouter" => "OPENROUTER_API_KEY",
            _ => "LLM_API_KEY",
        });
    std::env::var(env_var).is_ok()
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn cache_path_for(session_id: &str) -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("~"))
        .join(".mur")
        .join("session")
        .join("recordings")
        .join(format!("{}.extracted.json", session_id))
}

fn load_cached(path: &std::path::Path) -> Result<LlmExtractedJson> {
    let content = std::fs::read_to_string(path)?;
    let parsed: LlmExtractedJson = serde_json::from_str(&content)?;
    Ok(parsed)
}

fn save_cache(path: &std::path::Path, data: &LlmExtractedJson) -> Result<()> {
    let json = serde_json::to_string_pretty(data)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Build a transcript string from events, filtering noise and truncating to 4000 chars.
fn build_transcript(events: &[SessionEvent]) -> String {
    let noise_patterns = [
        "mur session start",
        "mur session stop",
        "mur session record",
        "mur sync",
        "mur context",
        "mur inject",
        "/mur:in",
        "/mur:out",
        "/mur-in",
        "/mur-out",
        "[stop:",
        "turn_end",
    ];

    let mut transcript = String::new();
    for evt in events {
        let c = evt.content.to_lowercase();
        if noise_patterns.iter().any(|n| c.contains(n)) || evt.content.trim().is_empty() {
            continue;
        }

        let role = match evt.event_type.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "tool_call" => {
                if let Some(ref t) = evt.tool {
                    &format!("Tool({})", t)
                } else {
                    "Tool"
                }
            }
            "tool_result" => "Result",
            _ => &evt.event_type,
        };

        // Truncate individual event content to keep things reasonable
        let content: String = evt.content.chars().take(500).collect();
        transcript.push_str(&format!("[{}] {}\n", role, content));

        if transcript.len() >= 4000 {
            transcript.truncate(4000);
            transcript.push_str("\n[...truncated]");
            break;
        }
    }

    transcript
}

/// Build a summary of the logic-extracted skeleton for the LLM prompt.
fn build_skeleton_summary(extracted: &ExtractedWorkflow) -> String {
    let w = &extracted.workflow;
    let mut summary = String::new();
    summary.push_str(&format!("Name: {}\n", w.base.name));
    summary.push_str(&format!("Description: {}\n", w.base.description));
    summary.push_str(&format!("Tools: {}\n", w.tools.join(", ")));
    summary.push_str(&format!("Steps ({}):\n", w.steps.len()));
    for step in &w.steps {
        summary.push_str(&format!("  {}. {}\n", step.order, step.description));
    }
    if !w.variables.is_empty() {
        summary.push_str("Variables:\n");
        for var in &w.variables {
            summary.push_str(&format!(
                "  - {} ({}): {}\n",
                var.name,
                format!("{:?}", var.var_type).to_lowercase(),
                var.description.as_deref().unwrap_or("")
            ));
        }
    }
    summary
}

/// Parse LLM response text into structured data, tolerant of markdown fences,
/// thinking tags, prefilled JSON, and other LLM output quirks.
fn parse_llm_response(response: &str) -> Option<LlmExtractedJson> {
    let trimmed = response.trim();

    // Try direct parse first
    if let Ok(parsed) = serde_json::from_str::<LlmExtractedJson>(trimmed) {
        return Some(parsed);
    }

    // Try with prefill restoration (assistant prefill starts with `{`)
    let with_brace = format!("{{{}", trimmed);
    if let Ok(parsed) = serde_json::from_str::<LlmExtractedJson>(&with_brace) {
        return Some(parsed);
    }

    // Try stripping markdown code fences
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();

    if let Ok(parsed) = serde_json::from_str::<LlmExtractedJson>(stripped) {
        return Some(parsed);
    }

    // Try prefill restoration on stripped content too
    let with_brace_stripped = format!("{{{}", stripped);
    if let Ok(parsed) = serde_json::from_str::<LlmExtractedJson>(&with_brace_stripped) {
        return Some(parsed);
    }

    // Fallback: find the first {...} JSON object in the response
    // (handles cases where LLM adds preamble, thinking tags, etc.)
    if let Some(start) = stripped.find('{')
        && let Some(end) = stripped.rfind('}')
        && start < end
    {
        let extracted = &stripped[start..=end];
        if let Ok(parsed) = serde_json::from_str::<LlmExtractedJson>(extracted) {
            return Some(parsed);
        }
        // Try parsing as a generic JSON value and look for nested workflow
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(extracted) {
            // Check for wrapper objects like {"workflow_template": {...}}
            if let Some(obj) = val.as_object() {
                for (_key, inner) in obj {
                    if let Ok(parsed) = serde_json::from_value::<LlmExtractedJson>(inner.clone()) {
                        return Some(parsed);
                    }
                }
            }
        }
    }

    None
}

/// Build a full `ExtractedWorkflow` from the LLM's parsed output.
fn build_workflow_from_llm(
    session_id: &str,
    events: &[SessionEvent],
    llm: &LlmExtractedJson,
) -> ExtractedWorkflow {
    let step_strings = llm.step_strings();
    let steps: Vec<Step> = step_strings
        .iter()
        .enumerate()
        .map(|(i, desc)| Step {
            order: (i + 1) as u32,
            description: desc.clone(),
            ..Default::default()
        })
        .collect();

    let var_list = llm.variable_list();
    let variables: Vec<Variable> = var_list
        .iter()
        .map(|v| Variable {
            name: v.name.clone(),
            var_type: VarType::String,
            required: true,
            default: v.default_value.clone(),
            description: Some(v.description.clone()),
            choices: vec![],
        })
        .collect();

    // Get the first user message for content
    let first_user_msg = events
        .iter()
        .find(|e| e.event_type == "user" && !e.content.trim().is_empty())
        .map(|e| e.content.trim().to_string())
        .unwrap_or_default();

    let workflow = Workflow {
        base: KnowledgeBase {
            name: llm.name.clone(),
            description: llm.description.clone(),
            content: Content::Plain(first_user_msg),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        steps,
        tools: llm.tools.clone(),
        source_sessions: vec![session_id.to_string()],
        trigger: llm.trigger.clone(),
        variables,
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    };

    ExtractedWorkflow { workflow }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_llm_response_valid_json() {
        let json = r#"{"name":"test","description":"A test","steps":["do thing"],"tools":["Bash"],"variables":[],"trigger":"manual"}"#;
        let result = parse_llm_response(json);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.name, "test");
        assert_eq!(r.steps.len(), 1);
    }

    #[test]
    fn test_parse_llm_response_with_fences() {
        let json = "```json\n{\"name\":\"test\",\"description\":\"A test\",\"steps\":[],\"tools\":[],\"variables\":[],\"trigger\":\"\"}\n```";
        let result = parse_llm_response(json);
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_llm_response_invalid() {
        let result = parse_llm_response("This is not JSON at all");
        assert!(result.is_none());
    }

    #[test]
    fn test_build_transcript_truncation() {
        let events: Vec<SessionEvent> = (0..100)
            .map(|i| SessionEvent {
                timestamp: i * 1000,
                event_type: "user".to_string(),
                tool: None,
                content: "a".repeat(200),
                ..Default::default()
            })
            .collect();
        let transcript = build_transcript(&events);
        // Should be around 4000 chars + truncation marker
        assert!(transcript.len() <= 4100);
    }

    #[test]
    fn test_build_transcript_filters_noise() {
        let events = vec![
            SessionEvent {
                timestamp: 1000,
                event_type: "user".to_string(),
                tool: None,
                content: "mur session start".to_string(),
                ..Default::default()
            },
            SessionEvent {
                timestamp: 2000,
                event_type: "user".to_string(),
                tool: None,
                content: "find AirPods prices".to_string(),
                ..Default::default()
            },
        ];
        let transcript = build_transcript(&events);
        assert!(!transcript.contains("mur session start"));
        assert!(transcript.contains("find AirPods prices"));
    }
}
