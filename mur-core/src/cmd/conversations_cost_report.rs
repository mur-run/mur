//! `mur conversations cost-report` — aggregates LlmCallRecord JSONL files into
//! per-stage totals + estimated USD cost.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::conversations::backend::telemetry::LlmCallRecord;

#[derive(Debug, Default, Clone, Serialize)]
pub struct StageTotals {
    pub stage: String,
    pub provider: String,
    pub model: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub estimated_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct CostReport {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub stages: Vec<StageTotals>,
    pub total_usd: f64,
}

/// Parse `--since` as either a humantime duration ("7d", "30d", "1h", "2w")
/// or an RFC3339 timestamp.
pub fn parse_since(since: &str) -> Result<DateTime<Utc>> {
    if let Ok(ts) = since.parse::<DateTime<Utc>>() {
        return Ok(ts);
    }
    let (n_str, unit) = since.split_at(since.len() - 1);
    let n: i64 = n_str
        .parse()
        .map_err(|_| anyhow::anyhow!("bad --since: {since}"))?;
    let dur = match unit {
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        "w" => Duration::weeks(n),
        _ => anyhow::bail!("bad --since unit: {unit} (want h, d, w, or RFC3339 timestamp)"),
    };
    Ok(Utc::now() - dur)
}

pub fn aggregate(records: impl Iterator<Item = LlmCallRecord>) -> Vec<StageTotals> {
    let mut buckets: BTreeMap<(String, String, String), StageTotals> = BTreeMap::new();
    for rec in records {
        let key = (rec.stage.clone(), rec.provider.clone(), rec.model.clone());
        let entry = buckets.entry(key).or_insert_with(|| StageTotals {
            stage: rec.stage.clone(),
            provider: rec.provider.clone(),
            model: rec.model.clone(),
            ..Default::default()
        });
        entry.calls += 1;
        entry.input_tokens += rec.input_tokens;
        entry.output_tokens += rec.output_tokens;
        entry.cache_read_input_tokens += rec.cache_read_input_tokens;
        entry.cache_creation_input_tokens += rec.cache_creation_input_tokens;
    }
    let mut stages: Vec<StageTotals> = buckets.into_values().collect();
    for s in &mut stages {
        s.estimated_usd = estimate_cost(
            &s.model,
            s.input_tokens,
            s.output_tokens,
            s.cache_read_input_tokens,
            s.cache_creation_input_tokens,
        );
    }
    stages
}

fn price_table(model: &str) -> Option<(f64, f64, f64, f64)> {
    match model {
        m if m.starts_with("claude-haiku-4-5") => Some((1.00, 5.00, 1.25, 0.10)),
        m if m.starts_with("claude-opus-5") => Some((5.00, 25.00, 6.25, 0.50)),
        m if m.starts_with("claude-sonnet-5") => Some((3.00, 15.00, 3.75, 0.30)),
        m if m.starts_with("claude-sonnet-4-6") => Some((3.00, 15.00, 3.75, 0.30)),
        m if m.starts_with("claude-opus-4-7") => Some((15.00, 75.00, 18.75, 1.50)),
        m if m.starts_with("claude-opus-4-6") => Some((15.00, 75.00, 18.75, 1.50)),
        m if m.starts_with("gpt-4o-mini") => Some((0.15, 0.60, 0.0, 0.0)),
        m if m.starts_with("gpt-4o") => Some((2.50, 10.00, 0.0, 0.0)),
        m if m.starts_with("gpt-4.1-mini") => Some((0.40, 1.60, 0.0, 0.0)),
        m if m.starts_with("gpt-4.1") => Some((2.00, 8.00, 0.0, 0.0)),
        m if m.starts_with("gpt-5-mini") => Some((0.25, 2.00, 0.0, 0.0)),
        m if m.starts_with("gpt-5") => Some((1.25, 10.00, 0.0, 0.0)),
        m if m.starts_with("o3-mini") => Some((1.10, 4.40, 0.0, 0.0)),
        m if m.starts_with("o3") => Some((2.00, 8.00, 0.0, 0.0)),
        m if m.starts_with("gemini-3.6-flash") => Some((0.30, 2.50, 0.0, 0.0)),
        m if m.starts_with("gemini-3.5-pro") => Some((1.25, 10.00, 0.0, 0.0)),
        m if m.starts_with("gemini-2.5-flash") => Some((0.30, 2.50, 0.0, 0.0)),
        m if m.starts_with("gemini-2.5-pro") => Some((1.25, 10.00, 0.0, 0.0)),
        m if m.starts_with("gemini-pro-3") => Some((1.25, 10.00, 0.0, 0.0)),
        _ => None,
    }
}

fn estimate_cost(
    model: &str,
    in_tok: u64,
    out_tok: u64,
    cache_r: u64,
    cache_w: u64,
) -> Option<f64> {
    let (pi, po, pcw, pcr) = price_table(model)?;
    Some(
        (in_tok as f64 / 1_000_000.0) * pi
            + (out_tok as f64 / 1_000_000.0) * po
            + (cache_w as f64 / 1_000_000.0) * pcw
            + (cache_r as f64 / 1_000_000.0) * pcr,
    )
}

pub fn read_records_since(
    since: DateTime<Utc>,
    root_override: Option<&str>,
) -> Result<Vec<LlmCallRecord>> {
    let dir = crate::conversations::paths::telemetry_root(root_override);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let contents = std::fs::read_to_string(&path)?;
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<LlmCallRecord>(line) {
                Ok(r) if r.ts >= since => out.push(r),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(err = ?e, line = line, "skipping malformed telemetry line")
                }
            }
        }
    }
    Ok(out)
}

pub async fn cmd_cost_report(since: &str, json: bool, root_override: Option<&str>) -> Result<()> {
    let since_ts = parse_since(since)?;
    let records = read_records_since(since_ts, root_override)?;
    let stages = aggregate(records.into_iter());
    // Note: f64::sum() of an empty iter returns -0.0, which renders as "$-0.000".
    // Coerce to +0.0 by adding 0.0.
    let total_usd: f64 = stages.iter().filter_map(|s| s.estimated_usd).sum::<f64>() + 0.0;
    let report = CostReport {
        since: since_ts,
        until: Utc::now(),
        stages,
        total_usd,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "Per-stage totals (since {}, until {}):\n",
        report.since.format("%Y-%m-%d %H:%M UTC"),
        report.until.format("%Y-%m-%d %H:%M UTC")
    );
    println!(
        "  {:<18} {:<12} {:<27} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "stage", "provider", "model", "calls", "in_tok", "out_tok", "cache_r", "cache_w", "est_$"
    );
    println!("  {}", "─".repeat(115));
    for s in &report.stages {
        let cost_str = match s.estimated_usd {
            Some(c) if c > 0.0001 => format!("${:.3}", c),
            Some(_) => "$0.000".into(),
            None => "—".into(),
        };
        println!(
            "  {:<18} {:<12} {:<27} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8}",
            s.stage,
            s.provider,
            truncate_to(&s.model, 27),
            s.calls,
            human_count(s.input_tokens),
            human_count(s.output_tokens),
            human_count(s.cache_read_input_tokens),
            human_count(s.cache_creation_input_tokens),
            cost_str,
        );
    }
    println!("  {}", "─".repeat(115));
    println!("  TOTAL{:>110}\n", format!("${:.3}", report.total_usd));
    println!("(ollama calls are local — no cost shown)");
    Ok(())
}

fn truncate_to(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}

fn human_count(n: u64) -> String {
    if n == 0 {
        "—".into()
    } else if n < 1_000 {
        format!("{n}")
    } else if n < 1_000_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(
        stage: &str,
        provider: &str,
        model: &str,
        in_tok: u64,
        out_tok: u64,
        cache_r: u64,
        cache_w: u64,
    ) -> LlmCallRecord {
        LlmCallRecord {
            ts: Utc::now(),
            provider: provider.into(),
            model: model.into(),
            stage: stage.into(),
            input_tokens: in_tok,
            output_tokens: out_tok,
            cache_read_input_tokens: cache_r,
            cache_creation_input_tokens: cache_w,
            latency_ms: 100,
            stream: false,
            success: true,
        }
    }

    #[test]
    fn parse_since_accepts_relative_durations() {
        assert!(parse_since("7d").is_ok());
        assert!(parse_since("30d").is_ok());
        assert!(parse_since("1h").is_ok());
        assert!(parse_since("2w").is_ok());
    }

    #[test]
    fn parse_since_accepts_rfc3339() {
        let p = parse_since("2026-04-01T00:00:00Z").unwrap();
        assert_eq!(p.format("%Y-%m-%d").to_string(), "2026-04-01");
    }

    #[test]
    fn parse_since_rejects_bad_unit() {
        assert!(parse_since("7x").is_err());
    }

    #[test]
    fn price_table_uses_published_per_mtok_rates() {
        // A wrong rate here is silent — the report still renders, just with the
        // wrong number — so the money path gets an explicit check.
        // Claude Opus 5 ships at the Opus 4.8 rate ($5/$25), NOT the $15/$75
        // the older Opus entries carry; cache write is 1.25x input, read 0.1x.
        assert_eq!(
            price_table("claude-opus-5"),
            Some((5.00, 25.00, 6.25, 0.50))
        );
        assert_eq!(
            price_table("claude-sonnet-5"),
            Some((3.00, 15.00, 3.75, 0.30))
        );
        // Prefix match, so a dated or suffixed variant resolves to the same row.
        assert_eq!(price_table("claude-opus-5"), price_table("claude-opus-5-x"));
        // Old IDs keep their own rates so historical records still price right.
        assert_eq!(
            price_table("claude-sonnet-4-6"),
            Some((3.00, 15.00, 3.75, 0.30))
        );
        assert_eq!(price_table("no-such-model"), None);
    }

    #[test]
    fn aggregate_groups_by_stage_provider_model() {
        let records = vec![
            rec("extractive", "anthropic", "claude-haiku-4-5", 100, 50, 0, 0),
            rec(
                "extractive",
                "anthropic",
                "claude-haiku-4-5",
                200,
                100,
                0,
                0,
            ),
            rec(
                "abstractive",
                "anthropic",
                "claude-haiku-4-5",
                1000,
                500,
                0,
                0,
            ),
            rec("ask.generate", "ollama", "qwen3:14b", 0, 0, 0, 0),
        ];
        let stages = aggregate(records.into_iter());
        assert_eq!(stages.len(), 3);
        let extr = stages.iter().find(|s| s.stage == "extractive").unwrap();
        assert_eq!(extr.calls, 2);
        assert_eq!(extr.input_tokens, 300);
        assert_eq!(extr.output_tokens, 150);
        assert!(extr.estimated_usd.is_some());
    }

    #[test]
    fn aggregate_sets_none_estimated_usd_for_ollama() {
        let records = vec![rec("rewriter", "ollama", "llama3.2:3b", 100, 50, 0, 0)];
        let stages = aggregate(records.into_iter());
        assert_eq!(stages.len(), 1);
        assert!(
            stages[0].estimated_usd.is_none(),
            "ollama models must not have a $ estimate"
        );
    }

    #[test]
    fn estimate_cost_haiku_matches_table() {
        let cost = estimate_cost("claude-haiku-4-5", 1_000_000, 1_000_000, 0, 0).unwrap();
        assert!((cost - 6.0).abs() < 0.001);
    }

    #[test]
    fn estimate_cost_openai_gpt4o_mini_matches_table() {
        // 1M in @ $0.15 + 1M out @ $0.60 = $0.75
        let cost = estimate_cost("gpt-4o-mini", 1_000_000, 1_000_000, 0, 0).unwrap();
        assert!((cost - 0.75).abs() < 0.001);
    }

    #[test]
    fn estimate_cost_gemini_25_flash_matches_table() {
        // 1M in @ $0.30 + 1M out @ $2.50 = $2.80
        let cost = estimate_cost("gemini-2.5-flash", 1_000_000, 1_000_000, 0, 0).unwrap();
        assert!((cost - 2.80).abs() < 0.001);
    }

    #[test]
    fn estimate_cost_unknown_model_returns_none() {
        assert!(estimate_cost("some-unknown-model-vNext", 1, 1, 0, 0).is_none());
    }

    #[test]
    fn read_records_since_filters_old_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("telemetry");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("llm-calls-test.jsonl");
        let old = LlmCallRecord {
            ts: Utc::now() - Duration::days(30),
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            stage: "rewriter".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            latency_ms: 100,
            stream: false,
            success: true,
        };
        let new = LlmCallRecord {
            ts: Utc::now() - Duration::hours(1),
            ..old.clone()
        };
        let body = format!(
            "{}\n{}\n",
            serde_json::to_string(&old).unwrap(),
            serde_json::to_string(&new).unwrap()
        );
        std::fs::write(&path, body).unwrap();
        let since = Utc::now() - Duration::days(7);
        let records = read_records_since(since, Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(records.len(), 1, "old record should be filtered out");
    }
}
