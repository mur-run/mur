//! `mur agent stats` and `mur agent logs` — telemetry and log inspection.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::resolve_mur_home;

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[allow(dead_code)]
pub struct TokenTotals {
    pub llm_calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Fold `gen_ai.usage.*` telemetry/*.jsonl under `agent_dir`.
#[allow(dead_code)]
pub fn agent_token_totals(agent_dir: &Path) -> TokenTotals {
    let mut t = TokenTotals::default();
    let telemetry_dir = agent_dir.join("telemetry");
    let Ok(entries) = std::fs::read_dir(&telemetry_dir) else {
        return t;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in body.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v.get("gen_ai.request.model").is_some() {
                t.llm_calls += 1;
                t.input_tokens += v["gen_ai.usage.input_tokens"].as_u64().unwrap_or(0);
                t.output_tokens += v["gen_ai.usage.output_tokens"].as_u64().unwrap_or(0);
            }
        }
    }
    t
}

pub fn cmd_stats(name: &str) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let dir = mur_home.join("agents").join(name);
    if !dir.exists() {
        bail!("agent '{name}' not found");
    }

    // Extract token/call totals via agent_token_totals.
    let totals = agent_token_totals(&dir);

    // Separate loop for latency and error metrics (not entangled with token counting).
    let telemetry_dir = dir.join("telemetry");
    let mut latency_total: u64 = 0;
    let mut errors: u64 = 0;

    if telemetry_dir.exists() {
        for entry in fs::read_dir(&telemetry_dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let body =
                fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            for line in body.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // LLM call rows carry the OTel `gen_ai.*` namespace.
                if v.get("gen_ai.request.model").is_some() {
                    latency_total += v["latency_ms"].as_u64().unwrap_or(0);
                }
                if v.get("kind").is_some() && v.get("recoverable").is_some() {
                    errors += 1;
                }
            }
        }
    }

    let avg_latency = latency_total.checked_div(totals.llm_calls).unwrap_or(0);
    println!("agent: {name}");
    println!("llm_calls: {}", totals.llm_calls);
    println!("input_tokens: {}", totals.input_tokens);
    println!("output_tokens: {}", totals.output_tokens);
    println!("avg_latency_ms: {avg_latency}");
    println!("errors: {errors}");
    Ok(())
}

pub fn cmd_logs(name: &str, tail: usize) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let dir = mur_home.join("agents").join(name);
    if !dir.exists() {
        bail!("agent '{name}' not found");
    }
    let log_path = dir.join("stderr.log");
    if !log_path.exists() {
        eprintln!("(no stderr.log for '{name}' yet)");
        return Ok(());
    }
    let body =
        fs::read_to_string(&log_path).with_context(|| format!("read {}", log_path.display()))?;
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(tail);
    for line in &lines[start..] {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_totals_folds_gen_ai_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let tdir = tmp.path().join("telemetry");
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(
            tdir.join("a.jsonl"),
            concat!(
                r#"{"gen_ai.request.model":"m","gen_ai.usage.input_tokens":100,"gen_ai.usage.output_tokens":40}"#,
                "\n",
                r#"{"not":"an llm row"}"#,
                "\n",
                r#"{"gen_ai.request.model":"m","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5}"#,
            ),
        )
        .unwrap();
        let t = agent_token_totals(tmp.path());
        assert_eq!((t.llm_calls, t.input_tokens, t.output_tokens), (2, 110, 45));
    }
}
