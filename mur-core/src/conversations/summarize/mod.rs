//! Sleep-time compact pipeline (Phase 2A, spec §4).
//!
//! Produces daily hybrid summaries: frontmatter + extractive spans +
//! abstractive narrative + macro expansion map. See
//! `docs/superpowers/specs/2026-04-20-mur-conversations-phase-2-design.md`.
#![allow(dead_code)] // public API wired progressively across Tasks 4-10.

pub mod abstractive;
pub mod chunker;
pub mod extractive;
pub mod macro_refs;
pub mod writer;

use anyhow::Result;
use chrono::{NaiveDate, Utc};
use mur_common::config::CompactConfig;
use sha2::{Digest, Sha256};
use std::time::Instant;
use tracing::info_span;

use super::audit::{self, AuditAction};
use super::ollama::OllamaClient;
use super::paths::summary_paths_for;
use super::store;

#[derive(Debug, Default)]
pub struct CompactReport {
    pub ok: u32,
    pub err: u32,
    pub skipped: u32,
    pub day_reports: Vec<DayReport>,
}

#[derive(Debug)]
pub struct DayReport {
    pub date: NaiveDate,
    pub outcome: Outcome,
    pub extractive_spans: u32,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub enum Outcome {
    Written { archived: bool },
    Noop,
    Skipped { reason: &'static str },
    Failed(String),
}

pub async fn compact_day(
    date: NaiveDate,
    force: bool,
    cfg: &CompactConfig,
    root_override: Option<&str>,
) -> Result<DayReport> {
    let _span = info_span!("compact.day", %date).entered();
    let start = Instant::now();

    let (md_path, _) = summary_paths_for(date, root_override);
    let msgs = store::read_day(date, root_override)?;
    if msgs.is_empty() {
        return Ok(DayReport {
            date,
            outcome: Outcome::Skipped {
                reason: "no raw for day",
            },
            extractive_spans: 0,
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Compute input_content_sha first — used for --if-stale guard.
    let input_sha = compute_input_sha(&msgs);

    // Skip if summary exists, is fresh, and not forced.
    if md_path.exists()
        && !force
        && let Ok(existing) = std::fs::read_to_string(&md_path)
        && existing.contains(&format!("input_content_sha: {}", input_sha))
    {
        return Ok(DayReport {
            date,
            outcome: Outcome::Skipped {
                reason: "already fresh",
            },
            extractive_spans: 0,
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Chunk + extract per chunk
    let client = OllamaClient::new(&cfg.ollama_endpoint, std::time::Duration::from_secs(120));
    let chunks = chunker::chunk_day(&msgs, cfg.chunk_tokens as usize);
    let mut all_spans = Vec::new();
    for chunk in &chunks {
        let spans = extractive::extract_chunk(&client, &cfg.extractive_model, chunk, &msgs).await?;
        all_spans.extend(spans);
    }

    // Dedup (reuse Phase 1 MinHash pattern via simple string equality for now;
    // structural dedup is Phase 2C polish)
    all_spans.sort_by_key(|a| a.line_hint);
    all_spans.dedup_by(|a, b| a.text == b.text && a.line_hint == b.line_hint);

    // Cap
    if all_spans.len() > cfg.max_extractive_spans as usize {
        all_spans.truncate(cfg.max_extractive_spans as usize);
    }

    // Abstractive
    let abstractive_result = abstractive::summarize(
        &client,
        &cfg.abstractive_model,
        &all_spans,
        date,
        cfg.max_abstractive_words,
    )
    .await;

    let mut warnings = Vec::new();
    if abstractive_result.narrative.is_none() {
        warnings.push("narrative_generation_failed".to_string());
    }

    // Macro refs
    let patterns_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".mur")
        .join("patterns");
    let mut abstractive_text = abstractive_result
        .narrative
        .clone()
        .unwrap_or_else(|| "(narrative unavailable)".into());
    let pattern_refs =
        macro_refs::detect_and_rewrite(&mut all_spans, &mut abstractive_text, &patterns_dir)
            .unwrap_or_default();
    let abstractive_final = abstractive::AbstractiveResult {
        narrative: Some(abstractive_text),
        word_count: abstractive_result.word_count,
    };

    // Frontmatter derived fields
    let sources = {
        let mut s: Vec<String> = msgs
            .iter()
            .map(|m| m.src.file_prefix().to_string())
            .collect();
        s.sort();
        s.dedup();
        s
    };
    let conv_count = {
        let mut c: Vec<&str> = msgs.iter().map(|m| m.conv.as_str()).collect();
        c.sort();
        c.dedup();
        c.len() as u32
    };
    let keywords = top_keywords(&all_spans, 10);

    let doc = writer::SummaryDoc {
        date,
        generated_at: Utc::now(),
        extractive_model: cfg.extractive_model.clone(),
        abstractive_model: cfg.abstractive_model.clone(),
        mur_version: env!("CARGO_PKG_VERSION").into(),
        duration_ms: start.elapsed().as_millis() as u64,
        conv_count,
        msg_count: msgs.len() as u32,
        sources,
        pattern_refs,
        keywords,
        links_prev: Some(date - chrono::Duration::days(1)),
        links_next: Some(date + chrono::Duration::days(1)),
        warnings,
        input_content_sha: input_sha,
        extractive: all_spans.clone(),
        abstractive: abstractive_final,
    };

    // Summary embedding: use a deterministic zero vector when MUR_OLLAMA_MOCK=1;
    // otherwise call the configured embedding provider via existing pipeline.
    let embed_dims = 1024_usize; // default; Phase 3 can read from cfg
    let summary_embedding = if OllamaClient::mock_from_env() {
        vec![0.1; embed_dims]
    } else {
        let text = doc
            .abstractive
            .narrative
            .as_deref()
            .unwrap_or("")
            .to_string();
        // Load global config to pick up embedding provider/model/dims.
        // If loading fails (no ~/.mur/config.yaml yet), fall back to a zero vector;
        // LanceDB row still writes, retrieve reranking just won't benefit from
        // semantic similarity until a user config exists or `mur reindex` is run.
        match crate::store::config::load_config() {
            Ok(cfg) => {
                let embed_cfg = crate::store::embedding::EmbeddingConfig::from_config(&cfg);
                crate::store::embedding::embed(&text, &embed_cfg)
                    .await
                    .unwrap_or_else(|_| vec![0.0; embed_dims])
            }
            Err(_) => vec![0.0; embed_dims],
        }
    };

    match writer::write_summary(&doc, summary_embedding, root_override).await {
        Ok(w) => Ok(DayReport {
            date,
            outcome: if w.noop {
                Outcome::Noop
            } else {
                Outcome::Written {
                    archived: w.archived.is_some(),
                }
            },
            extractive_spans: doc.extractive.len() as u32,
            duration_ms: doc.duration_ms,
        }),
        Err(e) => {
            // Record the failure in audit but don't throw — caller still gets a report.
            let _ = audit::Audit::open(root_override).and_then(|a| {
                a.append(
                    AuditAction::Error {
                        layer: "compact.write".into(),
                        reason: format!("{e:#}"),
                    },
                    String::new(),
                )
            });
            Ok(DayReport {
                date,
                outcome: Outcome::Failed(format!("{e:#}")),
                extractive_spans: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            })
        }
    }
}

pub async fn compact_missing(
    cfg: &CompactConfig,
    since: Option<NaiveDate>,
    if_stale: bool,
    max_days_override: Option<u32>,
    root_override: Option<&str>,
) -> Result<CompactReport> {
    let max_days = max_days_override.unwrap_or(cfg.max_days_per_run) as usize;
    let today = Utc::now().date_naive();

    let mut candidates: Vec<NaiveDate> = store::list_raw_dirs(root_override)?
        .into_iter()
        .map(|(d, _)| d)
        .filter(|d| *d < today)
        .filter(|d| since.is_none_or(|s| *d >= s))
        .collect();
    candidates.sort();

    let mut report = CompactReport::default();
    for date in candidates.into_iter().take(max_days) {
        // skip logic: if summary exists and --if-stale is off, skip
        let (md_path, _) = summary_paths_for(date, root_override);
        let force = if_stale;
        if md_path.exists() && !if_stale {
            report.skipped += 1;
            report.day_reports.push(DayReport {
                date,
                outcome: Outcome::Skipped {
                    reason: "summary exists",
                },
                extractive_spans: 0,
                duration_ms: 0,
            });
            continue;
        }
        let r = compact_day(date, force, cfg, root_override).await?;
        match &r.outcome {
            Outcome::Written { .. } | Outcome::Noop => report.ok += 1,
            Outcome::Failed(_) => report.err += 1,
            Outcome::Skipped { .. } => report.skipped += 1,
        }
        report.day_reports.push(r);
    }
    Ok(report)
}

fn compute_input_sha(msgs: &[mur_common::Message]) -> String {
    let mut h = Sha256::new();
    for m in msgs {
        h.update(serde_json::to_string(m).unwrap_or_default().as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

fn top_keywords(spans: &[extractive::ExtractiveSpan], n: usize) -> Vec<String> {
    use std::collections::HashMap;
    // Tiny TF heuristic, no TF-IDF in Phase 2A (corpus-level IDF is Phase 3).
    let mut counts: HashMap<String, usize> = HashMap::new();
    for s in spans {
        for w in s.text.split_whitespace() {
            let w = w.to_lowercase();
            if w.len() < 4 {
                continue;
            }
            // strip trailing punctuation
            let w = w
                .trim_end_matches(|c: char| !c.is_alphanumeric())
                .to_string();
            if w.is_empty() {
                continue;
            }
            *counts.entry(w).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<_> = counts.into_iter().collect();
    ranked.sort_by_key(|b| std::cmp::Reverse(b.1));
    ranked.into_iter().take(n).map(|(k, _)| k).collect()
}

#[cfg(test)]
mod orch_tests {
    use super::*;
    use chrono::{Datelike, TimeZone};
    use mur_common::{Content, Message, Role, Source};

    fn seed_raw(root: &str, date: NaiveDate, text: &str) {
        let ts = chrono::Utc
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 10, 0, 0)
            .unwrap();
        let m = Message {
            v: 1,
            ts,
            src: Source::ClaudeCode,
            conv: "c1".into(),
            role: Role::User,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        };
        store::append(&m, Some(root)).unwrap();
    }

    fn cfg() -> CompactConfig {
        CompactConfig::default()
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compact_day_happy_path_mock() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        seed_raw(root, date, "mock extractive span");
        let r = compact_day(date, false, &cfg(), Some(root)).await.unwrap();
        match r.outcome {
            Outcome::Written { .. } => {}
            other => panic!("expected Written, got {:?}", other),
        }
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compact_day_noop_when_fresh() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        seed_raw(root, date, "mock extractive span");
        let _ = compact_day(date, false, &cfg(), Some(root)).await.unwrap();
        let r2 = compact_day(date, false, &cfg(), Some(root)).await.unwrap();
        assert!(matches!(
            r2.outcome,
            Outcome::Skipped { .. } | Outcome::Noop
        ));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compact_missing_respects_throttle() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        for i in 1..=10 {
            let d = NaiveDate::from_ymd_opt(2026, 4, i).unwrap();
            seed_raw(root, d, &format!("day {i} mock extractive span"));
        }
        let mut c = cfg();
        c.max_days_per_run = 3;
        let report = compact_missing(&c, None, false, None, Some(root))
            .await
            .unwrap();
        assert_eq!(report.day_reports.len(), 3);
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
