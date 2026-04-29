//! Telemetry + logs read APIs (typed) for an agent.
//!
//! The CLI prints these as text; the GUI Logs window and Stats panel
//! consume them as structured values.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;

use crate::cmd::agent;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatsView {
    /// Summed counts per OTel-GenAI / mur.* event name.
    pub counters: BTreeMap<String, u64>,
    /// Number of telemetry files scanned.
    pub files_scanned: usize,
    /// Total bytes consumed.
    pub bytes_scanned: u64,
}

/// Aggregate telemetry counters from `<agent_home>/telemetry/*.jsonl`.
/// Mirrors `mur agent stats <name>` but returns the data instead of
/// printing.
pub fn stats(name: &str) -> Result<StatsView> {
    let mur_home = agent::resolve_mur_home()?;
    let dir = mur_home.join("agents").join(name).join("telemetry");
    let mut view = StatsView::default();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(view); // no telemetry yet
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        view.files_scanned += 1;
        view.bytes_scanned += text.len() as u64;
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
                *view.counters.entry(name.to_string()).or_insert(0) += 1;
            }
        }
    }
    Ok(view)
}

/// Read the last `tail` lines of `<agent_home>/stderr.log`. Returns
/// the lines as a single string with `\n` separators (no trailing
/// newline). If the file does not exist, returns the empty string.
pub fn logs(name: &str, tail: usize) -> Result<String> {
    let mur_home = agent::resolve_mur_home()?;
    let path = mur_home.join("agents").join(name).join("stderr.log");
    if !path.exists() {
        return Ok(String::new());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(tail);
    Ok(lines[start..].join("\n"))
}
