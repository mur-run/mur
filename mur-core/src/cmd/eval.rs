use anyhow::{Result, bail};
use chrono::{Duration, Utc};
use mur_common::config::RetrievalConfig;
use mur_common::knowledge::{KnowledgeBase, Maturity};
use mur_common::pattern::{Content, Pattern, Tier};
use serde::Serialize;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::evolve::maturity::evaluate_maturity;
use crate::retrieve::scoring::score_and_rank_with_config;
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
        "federation" => {
            println!("mur eval — federation suite");
            println!("{}", "═".repeat(30));
            println!("federation suite not yet implemented (requires E6 Agent Federation)");
            println!(
                "Run `mur eval run retrieval` and `mur eval run maturity` for available suites."
            );
            Ok(0)
        }
        other => {
            bail!("unknown suite '{other}' — valid: retrieval, maturity, reflector, federation")
        }
    }
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
    };

    let queries = test_queries();
    let mut hits = 0usize;
    let mut results: Vec<QueryResult> = Vec::new();

    for (query, expected) in &queries {
        let scored = score_and_rank_with_config(query, patterns.clone(), &config);
        let rank = scored
            .iter()
            .position(|sp| sp.pattern.name == *expected)
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
    let now = Utc::now();
    let mut results: Vec<(&'static str, bool, String)> = Vec::new();

    // ── Test 1: Draft baseline ──────────────────────────────────────────
    // New pattern with default evidence → maturity stays Draft (evaluate returns None)
    {
        let p = new_pattern("draft-baseline");
        let outcome = evaluate_maturity(&p, now);
        let pass = p.maturity == Maturity::Draft && outcome.is_none();
        let detail = if pass {
            String::new()
        } else {
            format!("expected Draft/None, got {:?}/{:?}", p.maturity, outcome)
        };
        results.push(("draft baseline", pass, detail));
    }

    // ── Test 2: Emerging promotion ──────────────────────────────────────
    // Draft pattern with injection_count >= 3 and override_signals == 0 → Emerging
    {
        let mut p = new_pattern("emerging-promotion");
        p.maturity = Maturity::Draft;
        p.evidence.injection_count = 3;
        p.evidence.success_signals = 3;
        p.evidence.override_signals = 0;

        let outcome = evaluate_maturity(&p, now);
        let pass = outcome == Some(Maturity::Emerging);
        let detail = if pass {
            String::new()
        } else {
            format!("expected Some(Emerging), got {:?}", outcome)
        };
        results.push(("emerging promotion", pass, detail));
    }

    // ── Test 3: Stable promotion ────────────────────────────────────────
    // Emerging pattern: injection_count >= 10, effectiveness >= 0.6, age >= 7 days → Stable
    {
        let mut p = new_pattern("stable-promotion");
        p.maturity = Maturity::Emerging;
        p.evidence.injection_count = 10;
        // effectiveness = 8/(8+2) = 0.8 >= 0.6
        p.evidence.success_signals = 8;
        p.evidence.override_signals = 2;
        // age = 10 days >= 7
        p.created_at = now - Duration::days(10);

        let outcome = evaluate_maturity(&p, now);
        let pass = outcome == Some(Maturity::Stable);
        let detail = if pass {
            String::new()
        } else {
            format!("expected Some(Stable), got {:?}", outcome)
        };
        results.push(("stable promotion", pass, detail));
    }

    // ── Test 4: No-op at Stable ─────────────────────────────────────────
    // Stable pattern with insufficient signals for Canonical and good enough
    // effectiveness to avoid demotion → stays Stable (evaluate returns None)
    {
        let mut p = new_pattern("stable-noop");
        p.maturity = Maturity::Stable;
        // effectiveness = 7/(7+3) = 0.7 >= 0.4 (no demotion)
        p.evidence.success_signals = 7;
        p.evidence.override_signals = 3;
        p.evidence.injection_count = 5; // < 30 (no promotion to Canonical)
        p.lifecycle.pinned = false;
        // recent activity so not considered inactive
        p.lifecycle.last_injected = Some(now);
        p.created_at = now - Duration::days(10);

        let outcome = evaluate_maturity(&p, now);
        let pass = outcome.is_none();
        let detail = if pass {
            String::new()
        } else {
            format!("expected None (no-op), got {:?}", outcome)
        };
        results.push(("no-op at stable", pass, detail));
    }

    // ── Report ───────────────────────────────────────────────────────────
    let total = results.len();
    let passed = results.iter().filter(|(_, ok, _)| *ok).count();
    let all_pass = passed == total;

    println!("mur eval — maturity suite");
    println!("{}", "═".repeat(30));
    for (name, ok, detail) in &results {
        if *ok {
            println!("  ✓ {name}");
        } else {
            println!("  ✗ {name}: {detail}");
        }
    }
    println!("{}", "─".repeat(30));
    let status = if all_pass { "PASS" } else { "FAIL" };
    println!("maturity: {passed}/{total}  {status}");

    Ok(if all_pass { 0 } else { 1 })
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
    fn test_federation_stub_returns_ok() {
        let code = cmd_eval_run("federation", "text").expect("federation stub should not error");
        assert_eq!(code, 0, "federation stub should return exit code 0");
    }
}
