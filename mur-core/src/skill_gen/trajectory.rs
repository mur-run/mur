//! Parse session recording JSONL into per-task trajectories.

use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Trajectory {
    pub task_summary: String,
    pub turns: Vec<Turn>,
    pub outcome: Outcome,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub kind: TurnKind,
    pub content: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone)]
pub enum TurnKind {
    UserPrompt,
    AgentMessage,
    ToolCall {
        tool: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool: String,
        ok: bool,
    },
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("malformed JSON on line {0}: {1}")]
    Json(usize, String),
    #[error("empty recording")]
    Empty,
}

/// Permissive envelope for forward-compat. Unknown event types are silently skipped.
#[derive(Debug, Deserialize)]
struct RawEvent {
    timestamp: Option<u64>,
    #[serde(rename = "type")]
    event_type: Option<String>,
    tool: Option<String>,
    content: Option<String>,
}

pub fn parse_recording(jsonl: &str) -> Result<Vec<Trajectory>, ParseError> {
    let lines: Vec<&str> = jsonl.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Err(ParseError::Empty);
    }

    let mut raw_events: Vec<RawEvent> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let ev: RawEvent =
            serde_json::from_str(line).map_err(|e| ParseError::Json(i + 1, e.to_string()))?;
        raw_events.push(ev);
    }

    let mut trajectories: Vec<Trajectory> = Vec::new();
    let mut current_turns: Vec<Turn> = Vec::new();
    let mut first_ts: Option<u64> = None;
    let mut last_ts: u64 = 0;
    let mut task_summary = String::new();

    for ev in &raw_events {
        let ts = ev.timestamp.unwrap_or(0);
        if first_ts.is_none() {
            first_ts = Some(ts);
        }
        last_ts = ts;

        let kind = match ev.event_type.as_deref() {
            Some("user") => TurnKind::UserPrompt,
            Some("tool_call") => {
                let tool = ev.tool.clone().unwrap_or_default();
                let input: serde_json::Value = ev
                    .content
                    .as_deref()
                    .and_then(|c| serde_json::from_str(c).ok())
                    .unwrap_or(serde_json::Value::Null);
                TurnKind::ToolCall { tool, input }
            }
            Some("tool_result") | Some("tool_response") => {
                let tool = ev.tool.clone().unwrap_or_default();
                let ok = ev
                    .content
                    .as_deref()
                    .map(|c| !c.contains("error") && !c.contains("Error"))
                    .unwrap_or(true);
                TurnKind::ToolResult { tool, ok }
            }
            Some("assistant") | Some("agent") => TurnKind::AgentMessage,
            Some("error") => {
                TurnKind::Error(ev.content.clone().unwrap_or_else(|| "unknown error".into()))
            }
            _ => continue, // skip unknown event types
        };

        // New trajectory boundary: fresh user prompt when we already have turns
        if matches!(kind, TurnKind::UserPrompt) && !current_turns.is_empty() {
            let outcome = classify_outcome(&current_turns);
            let duration = Duration::from_millis(last_ts - first_ts.unwrap_or(last_ts));
            trajectories.push(Trajectory {
                task_summary: task_summary.clone(),
                turns: std::mem::take(&mut current_turns),
                outcome,
                duration,
            });
            first_ts = Some(ts);
            task_summary.clear();
        }

        // Capture task_summary from the first user prompt
        if matches!(kind, TurnKind::UserPrompt) && task_summary.is_empty() {
            task_summary = ev
                .content
                .clone()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
        }

        let turn = Turn {
            kind,
            content: ev.content.clone().unwrap_or_default(),
            timestamp_ms: ts,
        };
        current_turns.push(turn);
    }

    // Final trajectory
    if !current_turns.is_empty() {
        let outcome = classify_outcome(&current_turns);
        let duration = Duration::from_millis(last_ts - first_ts.unwrap_or(last_ts));
        trajectories.push(Trajectory {
            task_summary,
            turns: current_turns,
            outcome,
            duration,
        });
    }

    Ok(trajectories)
}

fn classify_outcome(turns: &[Turn]) -> Outcome {
    let mut i = 0;
    while i < turns.len() {
        if let TurnKind::ToolResult { ok: false, .. } = &turns[i].kind {
            // Look ahead up to 2 turns for an Error
            for j in 1..=2 {
                if let Some(TurnKind::Error(msg)) =
                    turns.get(i + j).map(|t| &t.kind)
                {
                    return Outcome::Failure {
                        reason: msg.clone(),
                    };
                }
            }
        }
        i += 1;
    }
    Outcome::Success
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        assert!(matches!(parse_recording(""), Err(ParseError::Empty)));
    }

    #[test]
    fn single_success_trajectory() {
        let jsonl = r#"{"timestamp":1000,"type":"user","content":"find prices"}
{"timestamp":2000,"type":"tool_call","tool":"browser.navigate","content":"{\"url\":\"https://example.com\"}"}
{"timestamp":3000,"type":"tool_result","tool":"browser.navigate","content":"ok"}
{"timestamp":4000,"type":"assistant","content":"found it"}"#;
        let trajs = parse_recording(jsonl).unwrap();
        assert_eq!(trajs.len(), 1);
        assert_eq!(trajs[0].outcome, Outcome::Success);
        assert_eq!(trajs[0].task_summary, "find prices");
        assert_eq!(trajs[0].turns.len(), 4);
    }

    #[test]
    fn two_user_prompts_produce_two_trajectories() {
        let jsonl = r#"{"timestamp":1000,"type":"user","content":"task one"}
{"timestamp":2000,"type":"assistant","content":"done"}
{"timestamp":3000,"type":"user","content":"task two"}
{"timestamp":4000,"type":"assistant","content":"done"}"#;
        let trajs = parse_recording(jsonl).unwrap();
        assert_eq!(trajs.len(), 2);
        assert_eq!(trajs[0].task_summary, "task one");
        assert_eq!(trajs[1].task_summary, "task two");
    }

    #[test]
    fn tool_result_false_followed_by_error_is_failure() {
        let jsonl = r#"{"timestamp":1000,"type":"user","content":"do something"}
{"timestamp":2000,"type":"tool_call","tool":"api","content":"{}"}
{"timestamp":3000,"type":"tool_result","tool":"api","content":"error: timeout"}
{"timestamp":4000,"type":"error","content":"request timed out"}"#;
        let trajs = parse_recording(jsonl).unwrap();
        assert_eq!(trajs.len(), 1);
        assert_eq!(
            trajs[0].outcome,
            Outcome::Failure {
                reason: "request timed out".into()
            }
        );
    }

    #[test]
    fn unknown_event_types_skipped() {
        let jsonl = r#"{"timestamp":1000,"type":"user","content":"hello"}
{"timestamp":2000,"type":"weird_unknown_type","content":"ignore me"}
{"timestamp":3000,"type":"assistant","content":"world"}"#;
        let trajs = parse_recording(jsonl).unwrap();
        assert_eq!(trajs.len(), 1);
        assert_eq!(trajs[0].turns.len(), 2); // unknown skipped
    }
}
