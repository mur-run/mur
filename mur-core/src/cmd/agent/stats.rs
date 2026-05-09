//! `mur agent stats` and `mur agent logs` — telemetry and log inspection.

use std::fs;

use anyhow::{Context, Result, bail};

use super::resolve_mur_home;

pub fn cmd_stats(name: &str) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let dir = mur_home.join("agents").join(name);
    if !dir.exists() {
        bail!("agent '{name}' not found");
    }
    let telemetry_dir = dir.join("telemetry");

    let mut llm_calls: u64 = 0;
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
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
                    llm_calls += 1;
                    input_tokens += v["gen_ai.usage.input_tokens"].as_u64().unwrap_or(0);
                    output_tokens += v["gen_ai.usage.output_tokens"].as_u64().unwrap_or(0);
                    latency_total += v["latency_ms"].as_u64().unwrap_or(0);
                }
                if v.get("kind").is_some() && v.get("recoverable").is_some() {
                    errors += 1;
                }
            }
        }
    }

    let avg_latency = latency_total.checked_div(llm_calls).unwrap_or(0);
    println!("agent: {name}");
    println!("llm_calls: {llm_calls}");
    println!("input_tokens: {input_tokens}");
    println!("output_tokens: {output_tokens}");
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
