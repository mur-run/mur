//! Trace loading and clustering for LLM-augmented doctor checks (M6c).

use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;
use std::path::Path;

use super::{SkillTrace, TraceOutcome};

/// Load all skill traces from the JSONL store within `window`.
pub fn load_window(home: &Path, window: Duration) -> anyhow::Result<Vec<SkillTrace>> {
    let traces_dir = home.join("traces");
    if !traces_dir.exists() {
        return Ok(Vec::new());
    }

    let cutoff = Utc::now() - window;
    let mut traces = Vec::new();

    // Scan daily files from cutoff to now
    let mut day = cutoff.date_naive();
    let today = Utc::now().date_naive();
    while day <= today {
        let path = traces_dir
            .join(day.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        if path.exists()
            && let Ok(content) = std::fs::read_to_string(&path)
        {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || !trimmed.contains("mur.skill.executed") {
                    continue;
                }
                let Ok(val): Result<serde_json::Value, _> = serde_json::from_str(trimmed) else {
                    continue;
                };
                let skill_name = val
                    .get("mur.skill.name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if skill_name.is_empty() {
                    continue;
                }
                let ts = val
                    .get("ts")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.with_timezone(&Utc));
                let Some(timestamp) = ts else { continue };
                if timestamp < cutoff {
                    continue;
                }
                let outcome = match val
                    .get("mur.skill.outcome")
                    .and_then(|v| v.as_str())
                    .unwrap_or("not_evaluated")
                {
                    "success" => TraceOutcome::Success,
                    "failure" => TraceOutcome::Failure,
                    "cancelled" => TraceOutcome::Cancelled,
                    _ => TraceOutcome::Failure,
                };
                let skill_version = val
                    .get("mur.skill.version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let tools_used: Vec<String> = val
                    .get("mur.skill.tools")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let error = val
                    .get("mur.skill.error")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let trace_id = val
                    .get("trace_id")
                    .or(val.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                traces.push(SkillTrace {
                    skill_name,
                    skill_version,
                    outcome,
                    timestamp,
                    tools_used,
                    error,
                    trace_id,
                });
            }
        }
        day += chrono::Duration::days(1);
    }

    Ok(traces)
}

/// Group traces by (skill_name, skill_version). Stable order: most-recent first
/// within each group.
#[allow(dead_code)]
pub fn group_by_skill(traces: Vec<SkillTrace>) -> BTreeMap<(String, String), Vec<SkillTrace>> {
    let mut groups: BTreeMap<(String, String), Vec<SkillTrace>> = BTreeMap::new();
    for trace in traces {
        groups
            .entry((trace.skill_name.clone(), trace.skill_version.clone()))
            .or_default()
            .push(trace);
    }
    // Sort each group: most recent first
    for traces in groups.values_mut() {
        traces.sort_by_key(|b| std::cmp::Reverse(b.timestamp));
    }
    groups
}

/// Cap each group to the N most-recent traces (keeps prompts small).
#[allow(dead_code)]
pub fn cap_per_skill(
    groups: BTreeMap<(String, String), Vec<SkillTrace>>,
    n: usize,
) -> BTreeMap<(String, String), Vec<SkillTrace>> {
    groups
        .into_iter()
        .map(|(k, mut v)| {
            v.truncate(n);
            (k, v)
        })
        .collect()
}

/// Load recent traces for a single skill (convenience for api-drift).
pub fn load_recent_for(
    home: &Path,
    skill_name: &str,
    limit: usize,
) -> anyhow::Result<Vec<SkillTrace>> {
    let all = load_window(home, Duration::days(30))?;
    let mut matching: Vec<SkillTrace> = all
        .into_iter()
        .filter(|t| t.skill_name == skill_name)
        .collect();
    matching.sort_by_key(|b| std::cmp::Reverse(b.timestamp));
    matching.truncate(limit);
    Ok(matching)
}
