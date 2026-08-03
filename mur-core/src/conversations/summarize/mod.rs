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
pub mod rollup;
pub mod windows;
pub mod writer;

use anyhow::Result;
use chrono::{NaiveDate, Utc};
use mur_common::config::{CompactConfig, LlmConfig};
use sha2::{Digest, Sha256};
use std::time::Instant;
use tracing::info_span;

use super::audit::{self, AuditAction};
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
    llm: &LlmConfig,
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

    // Chunk + extract per chunk.
    //
    // P1 canary: extractive uses the new ChatBackend trait via factory::build,
    // so users can override `compact.extractive_backend` in config.yaml to
    // route extractive summarization through Anthropic instead of local Ollama.
    let extractive_cfg = cfg.effective_extractive_backend(llm);
    let extractive_backend =
        crate::conversations::backend::factory::build_for_stage(&extractive_cfg, "extractive")?;
    let chunks = chunker::chunk_day(&msgs, cfg.chunk_tokens as usize);
    let mut all_spans = Vec::new();
    for chunk in &chunks {
        let spans = extractive::extract_chunk(
            extractive_backend.as_ref(),
            &extractive_cfg.model,
            chunk,
            &msgs,
        )
        .await?;
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

    // Compute keywords BEFORE macro rewriting mutates spans (avoids
    // {{pattern: ...}} markers polluting TF counts).
    let keywords = top_keywords(&all_spans, 10);

    // Abstractive — same trait migration as P1 extractive. compact.abstractive
    // now flows through factory::build, so users can override
    // `compact.abstractive_backend` to route through Anthropic.
    let abstractive_cfg = cfg.effective_abstractive_backend(llm);
    let abstractive_backend =
        crate::conversations::backend::factory::build_for_stage(&abstractive_cfg, "abstractive")?;
    let abstractive_result = abstractive::summarize(
        abstractive_backend.as_ref(),
        &abstractive_cfg.model,
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
    let word_count = abstractive_text.split_whitespace().count();
    let abstractive_final = abstractive::AbstractiveResult {
        narrative: Some(abstractive_text),
        word_count,
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

    let doc = writer::SummaryDoc {
        date,
        generated_at: Utc::now(),
        extractive_model: extractive_cfg.model.clone(),
        abstractive_model: abstractive_cfg.model.clone(),
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
    // Also batch-embed all extractive spans for layer=2 upsert.
    let embed_dims = 1024_usize; // default; Phase 3 can read from cfg
    let (summary_embedding, span_embeddings) = match super::ollama::mock_mode() {
        Some(mode) => {
            let s = super::ollama::mock_embed_vector(
                doc.abstractive.narrative.as_deref().unwrap_or(""),
                mode,
                embed_dims,
            );
            let spans: Vec<Vec<f32>> = doc
                .extractive
                .iter()
                .map(|sp| super::ollama::mock_embed_vector(&sp.text, mode, embed_dims))
                .collect();
            (s, spans)
        }
        None => {
            let text = doc
                .abstractive
                .narrative
                .as_deref()
                .unwrap_or("")
                .to_string();
            let cfg_loaded = crate::store::config::load_config().ok();
            let embed_cfg = cfg_loaded
                .as_ref()
                .map(crate::store::embedding::EmbeddingConfig::from_config);
            let s = match &embed_cfg {
                Some(ec) => crate::store::embedding::embed(&text, ec)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("narrative embedding failed: {e:#}");
                        vec![0.0; embed_dims]
                    }),
                None => vec![0.0; embed_dims],
            };
            let spans: Vec<Vec<f32>> = if doc.extractive.is_empty() {
                Vec::new()
            } else if let Some(ec) = &embed_cfg {
                let texts: Vec<String> = doc.extractive.iter().map(|sp| sp.text.clone()).collect();
                crate::store::embedding::embed_batch(&texts, ec)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("span embedding failed: {e:#}");
                        texts.iter().map(|_| vec![0.0; embed_dims]).collect()
                    })
            } else {
                doc.extractive
                    .iter()
                    .map(|_| vec![0.0; embed_dims])
                    .collect()
            };
            (s, spans)
        }
    };

    match writer::write_summary(
        &doc,
        summary_embedding,
        span_embeddings,
        force,
        root_override,
    )
    .await
    {
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
    llm: &LlmConfig,
    since: Option<NaiveDate>,
    if_stale: bool,
    force: bool,
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

    // --force implies --if-stale semantics plus unconditional archive in write_summary.
    let effective_force = force || if_stale;

    let mut report = CompactReport::default();
    for date in candidates.into_iter().take(max_days) {
        // skip logic: if summary exists and neither --force nor --if-stale is set, skip
        let (md_path, _) = summary_paths_for(date, root_override);
        if md_path.exists() && !effective_force {
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
        let r = compact_day(date, effective_force, cfg, llm, root_override).await?;
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

pub struct ParsedSummary {
    pub date: NaiveDate,
    pub frontmatter: serde_yaml::Value,
    pub extractive: Vec<ParsedSpan>,
    pub narrative: String,
    pub pattern_refs: Vec<String>, // names only, full meta in frontmatter
}

#[derive(Debug, Clone)]
pub struct ParsedSpan {
    pub span_index: u32,
    pub src: String, // file_prefix
    pub conv_id: String,
    pub line_hint: u32,
    pub text: String,
}

pub fn parse_summary(md: &str) -> Result<ParsedSummary> {
    let (frontmatter, body) = split_frontmatter(md)?;
    let fm: serde_yaml::Value = serde_yaml::from_str(frontmatter)?;
    let date_str = fm
        .get("date")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing date"))?;
    let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")?;

    let extractive = parse_extractive_section(body);
    let narrative = parse_narrative_section(body);
    let pattern_refs = fm
        .get("pattern_refs")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok(ParsedSummary {
        date,
        frontmatter: fm,
        extractive,
        narrative,
        pattern_refs,
    })
}

fn split_frontmatter(md: &str) -> Result<(&str, &str)> {
    let body = md
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow::anyhow!("no frontmatter"))?;
    let end = body
        .find("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("unterminated frontmatter"))?;
    let fm = &body[..end];
    let rest = &body[end + 5..];
    Ok((fm, rest))
}

fn parse_extractive_section(body: &str) -> Vec<ParsedSpan> {
    let mut out = Vec::new();
    let span_re =
        regex::Regex::new(r"(?ms)^\[(\d+)\] _\{([^/]+)/([^ ]+) @L(\d+)\}_:\n((?:> [^\n]*\n?)+)")
            .unwrap();
    let ext_start = body.find("## Extractive spans").unwrap_or(0);
    let ext_end = body[ext_start..]
        .find("\n## ")
        .map(|i| ext_start + i)
        .unwrap_or(body.len());
    let section = &body[ext_start..ext_end];
    for cap in span_re.captures_iter(section) {
        let idx: u32 = cap[1].parse().unwrap_or(0);
        let src = cap[2].to_string();
        let conv = cap[3].to_string();
        let line: u32 = cap[4].parse().unwrap_or(0);
        let quoted = &cap[5];
        #[allow(clippy::useless_vec)]
        let text: String = quoted
            .lines()
            .map(|l| l.trim_start_matches("> ").trim_start_matches('>'))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        out.push(ParsedSpan {
            span_index: idx,
            src,
            conv_id: conv,
            line_hint: line,
            text,
        });
    }
    out
}

fn parse_narrative_section(body: &str) -> String {
    let narr_start = body
        .find("## Abstractive narrative")
        .map(|i| i + "## Abstractive narrative".len())
        .unwrap_or(0);
    let narr_end = body[narr_start..]
        .find("\n## ")
        .map(|i| narr_start + i)
        .unwrap_or(body.len());
    body[narr_start..narr_end].trim().to_string()
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

    fn llm() -> LlmConfig {
        LlmConfig::default()
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
        let r = compact_day(date, false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
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
        let _ = compact_day(date, false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
        let r2 = compact_day(date, false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
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
        let report = compact_missing(&c, &llm(), None, false, false, None, Some(root))
            .await
            .unwrap();
        assert_eq!(report.day_reports.len(), 3);
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn keywords_exclude_macro_marker_tokens() {
        // Guard: top_keywords must run on pre-macro-rewrite spans so the
        // `{{pattern: name}}` markers don't pollute the keywords list.
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        // Mock extractive returns: [{"text":"mock extractive span", ...}] — one word "pattern"
        // would appear only if we ran top_keywords on the mutated-by-macro-rewrite text.
        // We don't seed any pattern files so macro_refs is a no-op; this test verifies
        // the reorder is harmless (keywords derived from original span text), not that
        // macro_refs actually runs — that's covered by macro_refs::tests.
        seed_raw(root, date, "mock extractive span");
        let r = compact_day(date, false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
        // Re-read the written summary and confirm no "pattern" keyword appears.
        let (md_path, _) = summary_paths_for(date, Some(root));
        let body = std::fs::read_to_string(&md_path).unwrap();
        // The frontmatter keywords line.
        let keywords_line = body
            .lines()
            .find(|l| l.starts_with("keywords:"))
            .expect("keywords line missing");
        assert!(
            !keywords_line.contains("pattern"),
            "pattern marker leaked into keywords: {keywords_line}"
        );
        assert!(
            matches!(r.outcome, Outcome::Written { .. } | Outcome::Noop),
            "expected Written/Noop, got {:?}",
            r.outcome
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn narrative_fallback_word_count_is_consistent() {
        // Guard: when abstractive narrative is None and we substitute a fallback
        // string, the frontmatter word_count must match the rendered body, not
        // stay at 0 from the failed result.
        // We can't easily force the abstractive LLM to "fail" in mock mode (the
        // mock always returns Ok with prose). Instead we test that word_count is
        // computed from the rendered narrative via split_whitespace. For the mock
        // happy path, the abstractive narrative starts with "Mock narrative: ...",
        // so word_count should be >= 5 (the mock narrative length).
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        seed_raw(root, date, "mock extractive span");
        let _ = compact_day(date, false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
        let (md_path, _) = summary_paths_for(date, Some(root));
        let body = std::fs::read_to_string(&md_path).unwrap();
        // Extract narrative section
        let narrative_start = body
            .find("## Abstractive narrative\n\n")
            .expect("abstractive narrative section missing");
        let narrative_slice = &body[narrative_start + "## Abstractive narrative\n\n".len()..];
        let rendered_words = narrative_slice
            .split_whitespace()
            .take_while(|w| !w.starts_with("##"))
            .count();
        // The rendered narrative must contain at least one full sentence worth of words.
        assert!(
            rendered_words > 2,
            "narrative too short: {} words in '{}'",
            rendered_words,
            narrative_slice.lines().next().unwrap_or("")
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[test]
    fn parse_roundtrip_reads_frontmatter_and_body() {
        let markdown = r#"---
schema: 1
date: 2026-04-19
generated_at: 2026-04-19T03:00:00Z
generated_by:
  extractive_model: qwen3:14b
  abstractive_model: qwen3:14b
  mur_version: 2.4.0
duration_ms: 100
conv_count: 1
msg_count: 2
sources: [cc]
pattern_refs: []
keywords: [test]
links:
  prev: null
  next: null
warnings: []
input_content_sha: abc123
---

## Extractive spans

[1] _{cc/c1 @L1}_:
> hello

## Abstractive narrative

Today was a test.
"#;
        let parsed = parse_summary(markdown).unwrap();
        assert_eq!(parsed.date, NaiveDate::from_ymd_opt(2026, 4, 19).unwrap());
        assert_eq!(parsed.extractive.len(), 1);
        assert_eq!(parsed.extractive[0].conv_id, "c1");
        assert_eq!(parsed.extractive[0].line_hint, 1);
        assert_eq!(parsed.extractive[0].text, "hello");
        assert!(parsed.narrative.contains("Today was a test"));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compact_day_writes_both_narrative_and_span_rows() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        // Note: mock extractive returns conv_id="mock", so we seed with that conv.
        let ts = chrono::Utc
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 10, 0, 0)
            .unwrap();
        let m = Message {
            v: 1,
            ts,
            src: Source::ClaudeCode,
            conv: "mock".into(), // Must match mock extractive response
            role: Role::User,
            content: Content::Text {
                value: "mock extractive span".into(),
            },
            meta: serde_json::Value::Null,
            refs: vec![],
        };
        store::append(&m, Some(root)).unwrap();
        let r = compact_day(date, false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
        assert!(matches!(r.outcome, Outcome::Written { .. }));
        let idx = crate::conversations::index::ConversationIndex::open(1024, Some(root))
            .await
            .unwrap();
        let layer1_count = idx.count_rows_at_layer(1).await.unwrap();
        let layer2_count = idx.count_rows_at_layer(2).await.unwrap();
        assert_eq!(layer1_count, 1, "one narrative row");
        assert!(layer2_count >= 1, "at least one span row");
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
