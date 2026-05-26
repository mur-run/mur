//! Read agent telemetry JSONL, correlate with skill executions.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
struct TelemetryLine {
    #[serde(rename = "mur.event.type", default)]
    event_type: Option<String>,
    #[serde(rename = "mur.task.id", default)]
    task_id: Option<String>,
    #[serde(rename = "gen_ai.request.model", default)]
    model: Option<String>,
    #[serde(rename = "gen_ai.usage.input_tokens", default)]
    input_tokens: Option<u64>,
    #[serde(rename = "latency_ms", default)]
    latency_ms: Option<u64>,
    #[serde(rename = "tool", default)]
    tool: Option<String>,
    #[serde(rename = "ok", default)]
    ok: Option<bool>,
    #[serde(rename = "duration_ms", default)]
    duration_ms: Option<u64>,
    #[serde(rename = "mur.fired_skills", default)]
    fired_skills: Option<Vec<String>>,
    #[serde(rename = "message", default)]
    message: Option<String>,
    #[serde(rename = "kind", default)]
    #[allow(dead_code)]
    kind: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SkillExecution {
    pub skill_name: String,
    pub task_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub latency_ms: u64,
    pub tool_calls: Vec<ToolCallRecord>,
    pub errors: Vec<String>,
    pub was_successful: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ToolCallRecord {
    pub tool: String,
    pub ok: bool,
    pub duration_ms: u64,
}

/// Parse all telemetry files in `telemetry_dir`, return executions where
/// `fired_skills` includes `skill_name`. Scans at most `max_entries` LlmCall
/// events (most recent first via reverse file-scan).
pub fn read_skill_executions(
    telemetry_dir: &Path,
    skill_name: &str,
    max_entries: usize,
) -> Result<Vec<SkillExecution>> {
    if !telemetry_dir.exists() {
        return Ok(vec![]);
    }

    // Collect JSONL files sorted by name descending (dates).
    let mut files: Vec<_> = std::fs::read_dir(telemetry_dir)
        .context("read telemetry dir")?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    files.sort_by_key(|e| std::cmp::Reverse(e.file_name()));

    let mut executions: Vec<SkillExecution> = Vec::new();

    for entry in &files {
        if executions.len() >= max_entries {
            break;
        }
        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("read {}", entry.path().display()))?;
        for line in content.lines().rev() {
            if executions.len() >= max_entries {
                break;
            }
            if let Ok(tl) = serde_json::from_str::<TelemetryLine>(line)
                && tl.event_type.as_deref() == Some("telemetry/llm_call")
                && tl
                    .fired_skills
                    .as_ref()
                    .is_some_and(|fs| fs.iter().any(|s| s == skill_name))
            {
                executions.push(SkillExecution {
                    skill_name: skill_name.to_string(),
                    task_id: tl.task_id.unwrap_or_default(),
                    model: tl.model.unwrap_or_default(),
                    input_tokens: tl.input_tokens.unwrap_or(0),
                    latency_ms: tl.latency_ms.unwrap_or(0),
                    tool_calls: vec![],
                    errors: vec![],
                    was_successful: true,
                });
            }
        }
    }

    // Second pass (forward): correlate tool calls + errors with task IDs.
    // Build a set of tracked task IDs from the found executions.
    let tracked_tasks: std::collections::HashSet<String> =
        executions.iter().map(|e| e.task_id.clone()).collect();

    for entry in &files {
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for line in content.lines() {
            if let Ok(tl) = serde_json::from_str::<TelemetryLine>(line) {
                let tid = tl.task_id.as_deref().unwrap_or("");
                if !tracked_tasks.contains(tid) {
                    continue;
                }
                match tl.event_type.as_deref() {
                    Some("telemetry/tool_call") => {
                        if let Some(tool) = &tl.tool {
                            for exec in &mut executions {
                                if exec.task_id == tid {
                                    exec.tool_calls.push(ToolCallRecord {
                                        tool: tool.clone(),
                                        ok: tl.ok.unwrap_or(false),
                                        duration_ms: tl.duration_ms.unwrap_or(0),
                                    });
                                    if !tl.ok.unwrap_or(true) {
                                        exec.was_successful = false;
                                    }
                                }
                            }
                        }
                    }
                    Some("telemetry/error") => {
                        if let Some(msg) = &tl.message {
                            for exec in &mut executions {
                                if exec.task_id == tid {
                                    exec.errors.push(msg.clone());
                                    exec.was_successful = false;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(executions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_telemetry(dir: &Path, filename: &str, lines: &[&str]) {
        let content = lines.join("\n");
        std::fs::write(dir.join(filename), content).unwrap();
    }

    #[test]
    fn empty_telemetry_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_skill_executions(dir.path(), "my-skill", 50).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn single_match() {
        let dir = tempfile::tempdir().unwrap();
        write_telemetry(
            dir.path(),
            "2026-05-25.jsonl",
            &[
                r#"{"mur.event.type":"telemetry/llm_call","mur.task.id":"t1","mur.fired_skills":["target-skill"],"gen_ai.usage.input_tokens":100,"gen_ai.request.model":"claude","latency_ms":1200}"#,
            ],
        );
        let result = read_skill_executions(dir.path(), "target-skill", 50).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].skill_name, "target-skill");
        assert_eq!(result[0].task_id, "t1");
        assert!(result[0].was_successful);
    }

    #[test]
    fn non_matching_skill_filtered_out() {
        let dir = tempfile::tempdir().unwrap();
        write_telemetry(
            dir.path(),
            "2026-05-25.jsonl",
            &[
                r#"{"mur.event.type":"telemetry/llm_call","mur.task.id":"t1","mur.fired_skills":["other-skill"],"gen_ai.usage.input_tokens":50,"gen_ai.request.model":"claude","latency_ms":800}"#,
            ],
        );
        let result = read_skill_executions(dir.path(), "target-skill", 50).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn respects_max_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = Vec::new();
        for i in 1..=5 {
            lines.push(format!(r#"{{"mur.event.type":"telemetry/llm_call","mur.task.id":"t{i}","mur.fired_skills":["my-skill"],"gen_ai.usage.input_tokens":100,"gen_ai.request.model":"claude","latency_ms":1000}}"#));
        }
        write_telemetry(
            dir.path(),
            "2026-05-25.jsonl",
            &lines.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );
        let result = read_skill_executions(dir.path(), "my-skill", 2).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn tool_errors_set_was_successful_false() {
        let dir = tempfile::tempdir().unwrap();
        write_telemetry(
            dir.path(),
            "2026-05-25.jsonl",
            &[
                r#"{"mur.event.type":"telemetry/llm_call","mur.task.id":"t1","mur.fired_skills":["my-skill"],"gen_ai.usage.input_tokens":100,"gen_ai.request.model":"claude","latency_ms":1200}"#,
                r#"{"mur.event.type":"telemetry/tool_call","mur.task.id":"t1","tool":"wrong.tool","ok":false,"duration_ms":500}"#,
                r#"{"mur.event.type":"telemetry/error","mur.task.id":"t1","kind":"ToolError","message":"tool not found"}"#,
            ],
        );
        let result = read_skill_executions(dir.path(), "my-skill", 50).unwrap();
        assert_eq!(result.len(), 1);
        assert!(!result[0].was_successful);
        assert_eq!(result[0].errors.len(), 1);
        assert_eq!(result[0].tool_calls.len(), 1);
    }
}
