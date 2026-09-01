//! `mur agent routing` — what the router actually did, per turn.
//!
//! Smart background routing substitutes a cheaper model on turns nobody is
//! watching. Every decision is already recorded (`mur.routing` telemetry), but
//! until now the only place that rendered one was the Hub chat caption — and a
//! scheduled task or companion-outbox turn has no chat bubble to carry it. So
//! the decisions that are least visible were the ones with no read surface at
//! all.
//!
//! This is that surface. It answers one question the raw JSONL does not: how
//! many of these turns were chosen *for* you, and which model got them.

use std::path::Path;

use anyhow::{Result, bail};

/// One `mur.routing` record, narrowed to the fields worth showing.
struct Decision {
    ts: String,
    intent: String,
    model_ref: String,
    reason: String,
    outcome: String,
    attempts: u64,
    escalations: u64,
    summary: String,
}

impl Decision {
    /// MUR picked this model rather than being told to. The one class of turn
    /// this command exists for.
    fn is_downgrade(&self) -> bool {
        self.reason == REASON_SMART
    }
}

/// `reason` value the runtime writes when Smart chose the candidate list.
const REASON_SMART: &str = "smart-background";
/// Telemetry envelope discriminator for a routing decision.
const EVENT_TYPE: &str = "mur.routing";
/// Task summaries are stored truncated already; keep the table readable.
const SUMMARY_WIDTH: usize = 48;
/// Default rows shown when `--limit` is not given.
const DEFAULT_LIMIT: usize = 20;

fn parse_line(v: &serde_json::Value) -> Option<Decision> {
    if v.get("mur.event.type")?.as_str()? != EVENT_TYPE {
        return None;
    }
    Some(Decision {
        ts: v
            .get("ts")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        intent: v
            .get("intent")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        model_ref: v
            .get("model_ref")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        reason: v
            .get("reason")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        outcome: v
            .get("outcome")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        attempts: v.get("attempts").and_then(|t| t.as_u64()).unwrap_or(0),
        escalations: v.get("escalations").and_then(|t| t.as_u64()).unwrap_or(0),
        summary: v
            .get("task_summary")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// Every routing decision in `<agent_home>/telemetry/*.jsonl`, oldest first.
///
/// A malformed line is skipped rather than fatal: this is a log reader, and a
/// half-written trailing line is a normal thing to find in an append-only file
/// that a live process is still writing.
fn read_decisions(telemetry_dir: &Path) -> Result<Vec<Decision>> {
    let mut files: Vec<_> = std::fs::read_dir(telemetry_dir)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", telemetry_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    // Filenames are YYYY-MM-DD, so lexical order is chronological.
    files.sort();

    let mut out = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                && let Some(d) = parse_line(&v)
            {
                out.push(d);
            }
        }
    }
    Ok(out)
}

fn truncate(s: &str, max: usize) -> String {
    let one_line: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if one_line.chars().count() <= max {
        return one_line;
    }
    let kept: String = one_line.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// `mur agent routing <name> [--limit N] [--downgrades-only]`
pub fn cmd_routing(name: &str, limit: Option<usize>, downgrades_only: bool) -> Result<()> {
    let home = super::resolve_mur_home()?;
    let agent_dir = home.join("agents").join(name);
    if !agent_dir.exists() {
        bail!("agent '{name}' not installed at {}", agent_dir.display());
    }
    let telemetry_dir = agent_dir.join("telemetry");
    if !telemetry_dir.exists() {
        println!(
            "agent '{name}' has no telemetry yet ({})",
            telemetry_dir.display()
        );
        return Ok(());
    }

    let all = read_decisions(&telemetry_dir)?;
    // The summary counts EVERY decision on disk, not just the rows printed —
    // a "0 downgrades" line that only spoke for the last 20 turns would be the
    // same kind of half-truth this command exists to remove.
    let total = all.len();
    let downgrades = all.iter().filter(|d| d.is_downgrade()).count();

    let mut rows: Vec<&Decision> = if downgrades_only {
        all.iter().filter(|d| d.is_downgrade()).collect()
    } else {
        all.iter().collect()
    };
    rows.reverse(); // newest first
    let shown = limit.unwrap_or(DEFAULT_LIMIT).min(rows.len());

    if rows.is_empty() {
        println!("agent '{name}': no routing decisions recorded");
    } else {
        println!(
            "{:<20}  {:<22}  {:<24}  {:<17}  {:<15}  TASK",
            "TIME", "INTENT", "MODEL", "REASON", "OUTCOME"
        );
        for d in rows.iter().take(shown) {
            let mark = if d.is_downgrade() { "↓" } else { " " };
            let tries = if d.attempts > 1 || d.escalations > 0 {
                format!("{} ({}a/{}e)", d.outcome, d.attempts, d.escalations)
            } else {
                d.outcome.clone()
            };
            println!(
                "{:<20}  {:<22}  {}{:<23}  {:<17}  {:<15}  {}",
                truncate(&d.ts, 19),
                truncate(&d.intent, 22),
                mark,
                truncate(&d.model_ref, 23),
                truncate(&d.reason, 17),
                truncate(&tries, 15),
                truncate(&d.summary, SUMMARY_WIDTH)
            );
        }
    }

    println!();
    if downgrades == 0 {
        println!("{total} routing decisions · none were downgrades");
    } else {
        let models: std::collections::BTreeSet<&str> = all
            .iter()
            .filter(|d| d.is_downgrade())
            .map(|d| d.model_ref.as_str())
            .collect();
        println!(
            "{total} routing decisions · ↓ {downgrades} chosen for you by Smart, on {}",
            models.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    if !downgrades_only && rows.len() > shown {
        println!(
            "showing {shown} of {} — pass --limit to see more",
            rows.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_jsonl(dir: &Path, day: &str, lines: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{day}.jsonl")), lines.join("\n")).unwrap();
    }

    #[test]
    fn reads_routing_events_skips_everything_else_and_survives_a_torn_line() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("telemetry");
        write_jsonl(
            &dir,
            "2026-09-01",
            &[
                r#"{"mur.event.type":"mur.routing","ts":"2026-09-01T10:00:00Z","intent":"background/scheduled","model_ref":"cheap","reason":"smart-background","outcome":"ok","attempts":1,"escalations":0,"task_summary":"describe this photo"}"#,
                r#"{"mur.event.type":"mur.tool","ts":"2026-09-01T10:01:00Z"}"#,
                r#"{"mur.event.type":"mur.routing","ts":"2026-09-01T10:02:00Z","intent":"interactive","model_ref":"primary","reason":"fallback-advance","outcome":"ok","attempts":1,"escalations":0,"task_summary":"hello"}"#,
                // A live writer can leave a half-written trailing line.
                r#"{"mur.event.type":"mur.rou"#,
            ],
        );
        let got = read_decisions(&dir).unwrap();
        assert_eq!(got.len(), 2, "non-routing and torn lines are skipped");
        assert_eq!(got[0].reason, "smart-background");
        assert!(got[0].is_downgrade());
        assert!(
            !got[1].is_downgrade(),
            "fallback-advance is not a downgrade"
        );
    }

    #[test]
    fn days_are_read_in_chronological_order() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("telemetry");
        let line = |ts: &str| {
            format!(
                r#"{{"mur.event.type":"mur.routing","ts":"{ts}","intent":"interactive","model_ref":"m","reason":"explicit","outcome":"ok","attempts":1,"escalations":0,"task_summary":"x"}}"#
            )
        };
        write_jsonl(&dir, "2026-09-02", &[&line("2026-09-02T00:00:00Z")]);
        write_jsonl(&dir, "2026-08-31", &[&line("2026-08-31T00:00:00Z")]);
        let got = read_decisions(&dir).unwrap();
        assert_eq!(got.len(), 2);
        assert!(
            got[0].ts.starts_with("2026-08-31"),
            "oldest first, got {}",
            got[0].ts
        );
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // A CJK summary must not be cut mid-character.
        assert_eq!(truncate("辨識這張照片", 4), "辨識這…");
        assert_eq!(truncate("short", 40), "short");
        assert_eq!(
            truncate("a\nb", 40),
            "a b",
            "newlines would break the table"
        );
    }
}
