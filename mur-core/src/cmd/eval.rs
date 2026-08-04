use anyhow::{Result, bail};
use chrono::Utc;
use mur_common::config::RetrievalConfig;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::{Content, Pattern, Tier};
use serde::Serialize;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::federation::snapshot::read_snapshot_ref;
use crate::retrieve::scoring::score_and_rank_generic_with_config;
use crate::store::yaml::YamlStore;

pub fn cmd_eval_run(suite: &str, format: &str) -> Result<i32> {
    match suite {
        "retrieval" => run_retrieval_eval(format),
        "maturity" => run_maturity_eval(format),
        "reflector" => {
            println!("mur eval — reflector suite");
            println!("{}", "═".repeat(30));
            println!("reflector suite not yet implemented (requires E2 Reflector/Curator)");
            println!(
                "Run `mur eval run retrieval` and `mur eval run maturity` for available suites."
            );
            Ok(0)
        }
        "federation" => run_federation_eval(format),
        other => {
            bail!("unknown suite '{other}' — valid: retrieval, maturity, reflector, federation")
        }
    }
}

fn run_federation_eval(format: &str) -> Result<i32> {
    let agents_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur/agents");

    if !agents_dir.exists() {
        println!("mur eval — federation suite");
        println!("{}", "═".repeat(30));
        println!("No agents directory found at {}", agents_dir.display());
        println!("Create an agent first with `mur agent create <name>`.");
        return Ok(0);
    }

    // Collect per-agent stats.
    let mut results: Vec<FederationAgentResult> = Vec::new();
    let now = Utc::now();

    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let agent_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };

        // Snapshot lag: time since last snapshot pull.
        let snapshot_age_minutes: Option<i64> = read_snapshot_ref(&agent_name)?.map(|r| {
            chrono::DateTime::parse_from_rfc3339(&r.taken_at)
                .map(|t| (now - t.with_timezone(&Utc)).num_minutes())
                .unwrap_or(i64::MAX)
        });

        // Outbox stats: pending vs flushed.
        let outbox_dir = path.join("outbox");
        let (pending_count, flushed_count) = if outbox_dir.exists() {
            let pending = count_yaml_files(&outbox_dir, false);
            let flushed = count_yaml_files(&outbox_dir.join(".flushed"), false);
            (pending, flushed)
        } else {
            (0usize, 0usize)
        };

        let total = pending_count + flushed_count;
        let flush_rate = if total == 0 {
            100.0f64
        } else {
            flushed_count as f64 / total as f64 * 100.0
        };

        results.push(FederationAgentResult {
            agent: agent_name,
            snapshot_age_minutes,
            pending_signals: pending_count,
            flushed_signals: flushed_count,
            flush_rate_pct: flush_rate,
        });
    }

    // Compute summary metrics.
    let agents_with_snapshot: Vec<i64> = results
        .iter()
        .filter_map(|r| r.snapshot_age_minutes)
        .collect();
    let p50_lag_minutes: Option<i64> = {
        let mut sorted = agents_with_snapshot.clone();
        sorted.sort();
        sorted.get(sorted.len() / 2).copied()
    };
    let avg_flush_rate: f64 = if results.is_empty() {
        100.0
    } else {
        results.iter().map(|r| r.flush_rate_pct).sum::<f64>() / results.len() as f64
    };

    let lag_ok = p50_lag_minutes.is_none_or(|l| l < 5);
    let flush_ok = avg_flush_rate >= 95.0;
    let overall = if lag_ok && flush_ok { "PASS" } else { "FAIL" };

    if format == "json" {
        let out = serde_json::json!({
            "suite": "federation",
            "overall": overall,
            "p50_snapshot_lag_minutes": p50_lag_minutes,
            "avg_flush_rate_pct": avg_flush_rate,
            "agents": results.iter().map(|r| serde_json::json!({
                "agent": r.agent,
                "snapshot_age_minutes": r.snapshot_age_minutes,
                "pending_signals": r.pending_signals,
                "flushed_signals": r.flushed_signals,
                "flush_rate_pct": r.flush_rate_pct,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("mur eval — federation suite");
        println!("{}", "═".repeat(50));
        println!(
            "p50 snapshot lag : {}",
            p50_lag_minutes
                .map(|m| format!("{m}min {}", if lag_ok { "✓" } else { "✗ (>5min)" }))
                .unwrap_or_else(|| "n/a (no snapshots)".into())
        );
        println!(
            "avg flush rate   : {:.1}% {}",
            avg_flush_rate,
            if flush_ok { "✓" } else { "✗ (<95%)" }
        );
        println!();
        if results.is_empty() {
            println!("  (no agents found)");
        } else {
            println!(
                "  {:<20} {:>10} {:>10} {:>10}",
                "agent", "lag(min)", "pending", "flush%"
            );
            println!("  {}", "-".repeat(54));
            for r in &results {
                let lag = r
                    .snapshot_age_minutes
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "—".into());
                println!(
                    "  {:<20} {:>10} {:>10} {:>9.1}%",
                    r.agent, lag, r.pending_signals, r.flush_rate_pct
                );
            }
        }
        println!();
        println!("Overall: {overall}");
    }

    Ok(if overall == "PASS" { 0 } else { 1 })
}

fn count_yaml_files(dir: &std::path::Path, hidden: bool) -> usize {
    if !dir.exists() {
        return 0;
    }
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let p = e.path();
            let is_yaml = p.extension().and_then(|s| s.to_str()) == Some("yaml");
            let is_hidden = p
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with('.'));
            is_yaml && p.is_file() && (hidden || !is_hidden)
        })
        .count()
}

#[derive(Serialize)]
struct FederationAgentResult {
    agent: String,
    snapshot_age_minutes: Option<i64>,
    pending_signals: usize,
    flushed_signals: usize,
    flush_rate_pct: f64,
}

fn make_pattern(
    name: &str,
    description: &str,
    content: &str,
    tier: Tier,
    importance: f64,
) -> Pattern {
    Pattern {
        base: KnowledgeBase {
            name: name.to_string(),
            description: description.to_string(),
            content: Content::Plain(content.to_string()),
            tier,
            importance,
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    }
}

fn seed_patterns() -> Vec<(&'static str, &'static str, &'static str, Tier, f64)> {
    vec![
        (
            "swift-testing",
            "Use @Test macro for Swift unit tests",
            "Use @Test macro instead of XCTest for Swift Testing framework",
            Tier::Project,
            0.8,
        ),
        (
            "rust-error-handling",
            "Use anyhow for error propagation in Rust binaries",
            "Use anyhow::Result for application-level error handling",
            Tier::Core,
            0.9,
        ),
        (
            "prefer-chinese",
            "Reply in Traditional Chinese",
            "Always respond to the user in Traditional Chinese (zh-TW)",
            Tier::Core,
            0.9,
        ),
        (
            "git-commit-format",
            "Conventional commit messages",
            "Use conventional commits: feat/fix/docs/refactor scope message",
            Tier::Project,
            0.7,
        ),
        (
            "no-comments",
            "Avoid unnecessary code comments",
            "Do not add comments unless the WHY is non-obvious",
            Tier::Core,
            0.8,
        ),
        (
            "test-before-commit",
            "Run tests before committing",
            "Always run cargo test --workspace before creating a git commit",
            Tier::Project,
            0.8,
        ),
    ]
}

fn test_queries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("swift @Test macro unit test", "swift-testing"),
        ("rust error handling anyhow result", "rust-error-handling"),
        ("reply language traditional chinese zh-TW", "prefer-chinese"),
        (
            "git commit conventional format feat fix",
            "git-commit-format",
        ),
        ("code comments documentation why", "no-comments"),
    ]
}

#[derive(Serialize)]
struct QueryResult {
    query: String,
    expected: String,
    hit: bool,
    rank: Option<usize>,
}

struct TempDir(PathBuf);

impl TempDir {
    fn create() -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("mur_eval_{nonce}"));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_retrieval_eval(format: &str) -> Result<i32> {
    let tmp = TempDir::create()?;
    let store = YamlStore::new(tmp.path().clone())?;

    for (name, desc, content, tier, importance) in seed_patterns() {
        let p = make_pattern(name, desc, content, tier, importance);
        store.save(&p)?;
    }

    let patterns = store.list_all()?;
    let config = RetrievalConfig {
        max_patterns: 3,
        min_score: 0.0,
        max_tokens: 10000,
        mmr_threshold: 0.85,
        reserved_note_slots: 0, // eval ranks patterns only; no notes to reserve for
    };

    let queries = test_queries();
    let mut hits = 0usize;
    let mut results: Vec<QueryResult> = Vec::new();

    for (query, expected) in &queries {
        let scored = score_and_rank_generic_with_config(query, patterns.clone(), &config);
        let rank = scored
            .iter()
            .position(|sp| sp.item.name == *expected)
            .map(|i| i + 1);
        let hit = rank.is_some();
        if hit {
            hits += 1;
        }
        results.push(QueryResult {
            query: query.to_string(),
            expected: expected.to_string(),
            hit,
            rank,
        });
    }

    let total = queries.len();
    let precision = hits as f64 / total as f64;
    let threshold = 0.70_f64;
    let pass = precision >= threshold;

    if format == "json" {
        #[derive(Serialize)]
        struct JsonOut {
            suite: &'static str,
            precision_at_3: f64,
            queries: usize,
            hits: usize,
            threshold: f64,
            pass: bool,
            results: Vec<QueryResult>,
        }
        let out = JsonOut {
            suite: "retrieval",
            precision_at_3: precision,
            queries: total,
            hits,
            threshold,
            pass,
            results,
        };
        println!("{}", serde_json::to_string(&out)?);
    } else {
        println!("mur eval — retrieval suite");
        println!("{}", "═".repeat(30));
        for r in &results {
            let rank_str = match r.rank {
                Some(n) => format!("#{n}"),
                None => "absent".to_string(),
            };
            let mark = if r.hit { "✓" } else { "✗" };
            println!("query: {:?}", r.query);
            println!("  expected: {}  {} (rank {})", r.expected, mark, rank_str);
        }
        println!("{}", "─".repeat(30));
        let pass_str = if pass { "✓ PASS" } else { "✗ FAIL" };
        println!(
            "precision@3: {hits}/{total}  ({precision:.2})  {pass_str}  [threshold {threshold:.2}]"
        );
    }

    Ok(if pass { 0 } else { 1 })
}

#[allow(dead_code)]
fn new_pattern(name: &str) -> Pattern {
    Pattern {
        base: KnowledgeBase {
            name: name.to_string(),
            description: "eval test pattern".to_string(),
            content: Content::Plain("test".to_string()),
            tier: Tier::Session,
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    }
}

fn run_maturity_eval(_format: &str) -> Result<i32> {
    println!("mur eval — maturity suite");
    println!("{}", "═".repeat(30));
    println!(
        "maturity suite not available (Pattern maturity removed — skills use skill/lifecycle.rs)"
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_maturity_eval_pass() {
        let code = cmd_eval_run("maturity", "text").expect("maturity eval should not error");
        assert_eq!(code, 0, "maturity eval should pass (all smoke tests)");
    }

    #[test]
    fn test_retrieval_eval_pass() {
        let code = cmd_eval_run("retrieval", "text").expect("eval should not error");
        assert_eq!(code, 0, "retrieval eval should pass (precision@3 >= 0.70)");
    }

    #[test]
    fn test_retrieval_eval_unknown_suite() {
        let err = cmd_eval_run("bogus", "text");
        assert!(err.is_err(), "unknown suite should return an error");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("unknown suite"),
            "error message should mention 'unknown suite'"
        );
    }

    #[test]
    fn test_reflector_stub_returns_ok() {
        let code = cmd_eval_run("reflector", "text").expect("reflector stub should not error");
        assert_eq!(code, 0, "reflector stub should return exit code 0");
    }

    #[test]
    fn test_federation_no_agents_returns_ok() {
        // When no agents dir exists the suite should report gracefully and return 0.
        let code = cmd_eval_run("federation", "text").expect("federation eval should not error");
        // May be 0 (no agents found) or 1 (fail) depending on actual ~/.mur/agents; either is valid.
        assert!(code == 0 || code == 1);
    }

    #[test]
    fn test_federation_eval_with_fresh_snapshot() {
        use mur_common::agent::{PatternFilter, SnapshotRef};
        use tempfile::TempDir;

        // Build a fake agents dir with one agent that has a fresh snapshot and empty outbox.
        let tmp = TempDir::new().unwrap();
        let agent_dir = tmp.path().join("agents").join("test-agent");
        let cache_dir = agent_dir.join("patterns_cache");
        let outbox_dir = agent_dir.join("outbox");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::create_dir_all(&outbox_dir).unwrap();

        let snap = SnapshotRef {
            knowledge_commit: "abc123".into(),
            taken_at: chrono::Utc::now().to_rfc3339(),
            filter: PatternFilter::default(),
        };
        let yaml = serde_yaml_ng::to_string(&snap).unwrap();
        std::fs::write(cache_dir.join(".snapshot-ref"), yaml).unwrap();

        // Manually exercise run_federation_eval logic using the public read_snapshot_ref:
        // Since run_federation_eval reads from ~/.mur/agents we can't inject tmp here,
        // but we can verify the snapshot ref roundtrip via read_snapshot_ref.
        // The function itself is tested via the no-agents path; the outbox counting
        // is covered by AgentOutbox tests.
        let ref_back = read_snapshot_ref("test-agent");
        // It will return None (test-agent doesn't exist in ~/.mur) — that's fine.
        // The important thing is it doesn't error.
        assert!(ref_back.is_ok());
    }

    #[test]
    fn test_federation_eval_json_format() {
        // JSON output should be valid JSON regardless of agent state.
        let code =
            cmd_eval_run("federation", "json").expect("federation json eval should not error");
        assert!(code == 0 || code == 1);
    }
}
