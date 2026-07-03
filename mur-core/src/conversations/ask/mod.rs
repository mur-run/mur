//! Mode C — Ask: local-only RAG with inline citations. See spec §5.
#![allow(dead_code)] // filled progressively across Tasks 19-25

use mur_common::Source;
use std::sync::Arc;
use std::time::Duration;

use crate::conversations::backend::ChatBackend;

pub mod abstractive;
pub mod cache;
pub mod cite;
pub mod compress;
pub mod format;
pub mod generate;
pub mod prompt;
pub mod retrieve;
pub mod rewriter;
pub mod session;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Format {
    Plain,
    Json,
}

#[derive(Debug, Clone)]
pub struct Filters {
    pub source: Vec<Source>,
    pub since: Option<chrono::NaiveDate>,
    pub until: Option<chrono::NaiveDate>,
    pub min_score: f64,
}

#[derive(Clone)]
pub struct AskRequest {
    pub question: String,
    pub filters: Filters,
    pub k_summary: usize,
    pub k_raw: usize,
    pub escalation_threshold: f64,
    pub mmr_threshold: f64,
    pub model: String,
    pub endpoint: String,
    pub format: Format,
    pub max_context_tokens: usize,
    pub response_tokens: usize,
    pub timeout: Duration,
    pub no_escalate: bool,
    pub debug_prompt: bool,
    pub strict_citations: bool,
    pub prior_turns: Vec<session::TurnRecord>,
    /// The query actually used for retrieval. If `--continue` + rewriter ran,
    /// this differs from `question`. If `Skipped`, equals `question`.
    pub retrieval_query: String,
    pub rewriter_status: session::RewriterStatus,
    pub compress_enabled: bool,
    pub summarize_enabled: bool,
    pub summarize_model: Option<String>,
    /// Optional pre-built backend for the answer-generation streaming call.
    /// `None` = synthesize an Ollama backend from `endpoint` + `model` (the
    /// legacy path). Construct via
    /// `factory::build_for_stage(&ask_cfg.synthesize_backend(), "ask.generate")`
    /// at the CLI/API call site to honor the per-stage `ask.backend` override
    /// AND emit per-call cost telemetry.
    pub answer_backend: Option<Arc<dyn ChatBackend>>,
    /// Optional pre-built backend for the Phase 3.5 Stage 1b per-hit
    /// abstractive compression. Built separately from `answer_backend` so
    /// telemetry can attribute Stage 1b spend ("ask.compress_hit")
    /// distinctly from the answer stream ("ask.generate"). `None` =
    /// synthesize from `endpoint` + `model`.
    pub stage1b_backend: Option<Arc<dyn ChatBackend>>,
}

/// Provenance marker for hit snippets that have been reduced before going
/// into the LLM prompt. `Heuristic` → Phase 3.4 extractive compression (free).
/// `Abstractive` → Phase 3.5 LLM-summarized (paid). Written by the later
/// transformation, so a hit touched by both ends up marked `Abstractive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    Heuristic,
    Abstractive,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Citation {
    pub id: u32,
    pub date: chrono::NaiveDate,
    pub source: String, // file_prefix
    pub conv_id: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub snippet: String,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressed: Option<Compression>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HitInfo {
    pub layer: i8,
    pub source: String,
    pub conv_id: String,
    pub date: chrono::NaiveDate,
    pub score: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AskResponse {
    pub answer: String,
    pub citations: Vec<Citation>,
    pub hits_used: Vec<HitInfo>,
    pub degraded_to_mode_b: bool,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub duration_ms: u64,
    pub rewritten_question: Option<String>,
    pub rewriter_status: session::RewriterStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_1b: Option<abstractive::Stage1bStats>,
}

#[derive(Debug, Clone)]
pub enum AskEvent {
    Token(String),
    Citation(Citation),
    HitInfo(HitInfo),
    Done {
        tokens_in: usize,
        tokens_out: usize,
        degraded: bool,
        duration_ms: u64,
        stage_1b: Option<abstractive::Stage1bStats>,
    },
    Error(String),
}

use anyhow::Result;
use futures::stream::{Stream, StreamExt};
use std::pin::Pin;
use std::time::Instant;

pub async fn ask_stream(
    req: AskRequest,
    root_override: Option<&str>,
) -> Result<Pin<Box<dyn Stream<Item = Result<AskEvent>> + Send>>> {
    use async_stream::try_stream;

    let start = Instant::now();

    // 1. Embed
    let query_embedding = match embed_query(&req.retrieval_query).await {
        Ok(v) => v,
        Err(e) => {
            return Ok(Box::pin(try_stream! {
                yield AskEvent::Error(format!("embed failed: {e:#}"));
                yield AskEvent::Done {
                    tokens_in: 0,
                    tokens_out: 0,
                    degraded: false,
                    duration_ms: start.elapsed().as_millis() as u64,
                    stage_1b: None,
                };
            }));
        }
    };

    // 2. Retrieve
    let hits = match retrieve::gather_hits(retrieve::RetrieveArgs {
        query_embedding,
        filters: &req.filters,
        k_summary: req.k_summary,
        k_raw: req.k_raw,
        escalation_threshold: req.escalation_threshold,
        mmr_threshold: req.mmr_threshold,
        no_escalate: req.no_escalate,
        max_context_tokens: req.max_context_tokens,
        root_override,
    })
    .await
    {
        Ok(h) => h,
        Err(e) => {
            return Ok(Box::pin(try_stream! {
                yield AskEvent::Error(format!("retrieve failed: {e:#}"));
                yield AskEvent::Done {
                    tokens_in: 0,
                    tokens_out: 0,
                    degraded: false,
                    duration_ms: start.elapsed().as_millis() as u64,
                    stage_1b: None,
                };
            }));
        }
    };

    if hits.is_empty() {
        return Ok(Box::pin(try_stream! {
            yield AskEvent::Token("The conversations I have access to don't cover that.".into());
            yield AskEvent::Done {
                tokens_in: 0,
                tokens_out: 0,
                degraded: false,
                duration_ms: start.elapsed().as_millis() as u64,
                stage_1b: None,
            };
        }));
    }

    // 3. Build prompt (incl. Phase 3.5 Stage 1b when enabled).
    //
    // Stage 1b backend: P3 task 8 reverses the Task 4 dedup so per-call
    // telemetry can attribute Stage 1b spend ("ask.compress_hit") separately
    // from the answer stream ("ask.generate"). When the caller pre-built
    // backends (req.{stage1b,answer}_backend = Some(_)), use them as-is —
    // cmd_ask wraps both in TelemetryBackend with their own stage tag.
    // When None (legacy callers / tests), synthesize two backends from
    // req.endpoint / req.timeout, each tagged with its own stage.
    let synthesize = || -> mur_common::config::BackendConfig {
        mur_common::config::BackendConfig {
            provider: "ollama".into(),
            model: req.model.clone(),
            endpoint: Some(req.endpoint.clone()),
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: Some(req.timeout.as_secs()),
        }
    };
    let stage1b_backend: Arc<dyn ChatBackend> = match req.stage1b_backend.clone() {
        Some(b) => b,
        None => crate::conversations::backend::factory::build_for_stage(
            &synthesize(),
            "ask.compress_hit",
        )?,
    };

    let summarize_model_owned: Option<String> = req
        .summarize_model
        .clone()
        .or_else(|| Some(req.model.clone()));
    let abstractive_ctx_owned =
        summarize_model_owned
            .as_ref()
            .map(|m| abstractive::AbstractiveCtx {
                backend: stage1b_backend.as_ref(),
                model: m.as_str(),
                timeout: abstractive::CALL_TIMEOUT,
                root_override,
            });

    // Emit HitInfo events from the ORIGINAL hits vec (pre-compression) so
    // downstream session records still reflect retrieval state, not mutation.
    let hit_events: Vec<AskEvent> = hits
        .iter()
        .map(|h| AskEvent::HitInfo(h.info.clone()))
        .collect();

    let prompt = prompt::render(
        &req.question,
        &req.prior_turns,
        hits,
        req.max_context_tokens,
        req.response_tokens,
        req.compress_enabled,
        req.summarize_enabled,
        abstractive_ctx_owned.as_ref(),
    )
    .await;

    let stage_1b_stats = prompt.stage_1b.as_ref().map(|s| s.to_stats());

    // 4. Generate (streaming) with grounding filter.
    //
    // The answer backend is built via `factory::build_for_stage` at the call
    // site (cmd_ask) so per-stage `ask.backend` overrides reach us through
    // `req.answer_backend`. When None (legacy callers / tests), synthesize
    // a fresh Ollama backend tagged with the "ask.generate" stage so
    // telemetry distinguishes it from the Stage 1b backend above.
    let model = req.model.clone();
    let answer_backend: Arc<dyn ChatBackend> = match req.answer_backend.clone() {
        Some(b) => b,
        None => {
            crate::conversations::backend::factory::build_for_stage(&synthesize(), "ask.generate")?
        }
    };
    let filter = if req.strict_citations {
        cite::GroundingFilter::new_strict(prompt.valid_citations.clone())
    } else {
        cite::GroundingFilter::new(prompt.valid_citations.clone())
    };
    let tokens_in = prompt.tokens_est;

    let stream = match generate::stream_answer(
        answer_backend.as_ref(),
        &model,
        &prompt.system,
        &prompt.user,
        req.response_tokens as u32,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            let mode_b = hits_as_mode_b(&prompt.final_hits);
            let stage_1b_err = stage_1b_stats.clone();
            return Ok(Box::pin(try_stream! {
                for evt in hit_events { yield evt; }
                yield AskEvent::Token(mode_b);
                yield AskEvent::Done {
                    tokens_in,
                    tokens_out: 0,
                    degraded: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                    stage_1b: stage_1b_err,
                };
                yield AskEvent::Error(format!("ollama unavailable: {e:#}"));
            }));
        }
    };

    let citation_events_by_anchor = citations_map(&prompt.final_hits);
    let out_stream = try_stream! {
        for evt in hit_events { yield evt; }
        let mut stream = stream;
        let mut filter = filter;
        let mut tokens_out = 0usize;
        let mut emitted_citations = std::collections::HashSet::new();
        while let Some(next) = stream.next().await {
            let tok = next?;
            let filtered = filter.feed(&tok);
            if !filtered.forwarded.is_empty() {
                tokens_out += filtered.forwarded.len() / 4 + 1;
                for c in citations_fired_in(&filtered.forwarded, &citation_events_by_anchor) {
                    if emitted_citations.insert(c.id) {
                        yield AskEvent::Citation(c.clone());
                    }
                }
                yield AskEvent::Token(filtered.forwarded);
            }
        }
        let drained = filter.flush();
        if !drained.forwarded.is_empty() {
            tokens_out += drained.forwarded.len() / 4 + 1;
            for c in citations_fired_in(&drained.forwarded, &citation_events_by_anchor) {
                if emitted_citations.insert(c.id) {
                    yield AskEvent::Citation(c.clone());
                }
            }
            yield AskEvent::Token(drained.forwarded);
        }
        if let Err(e) = filter.coverage_check() {
            yield AskEvent::Error(e);
        }
        yield AskEvent::Done {
            tokens_in,
            tokens_out,
            degraded: false,
            duration_ms: start.elapsed().as_millis() as u64,
            stage_1b: stage_1b_stats.clone(),
        };
    };
    Ok(Box::pin(out_stream))
}

pub async fn ask(req: AskRequest, root_override: Option<&str>) -> Result<AskResponse> {
    let retrieval_query = req.retrieval_query.clone();
    let rewriter_status = req.rewriter_status;
    let mut stream = ask_stream(req, root_override).await?;
    let mut answer = String::new();
    let mut citations = Vec::new();
    let mut hits_used = Vec::new();
    let mut degraded = false;
    let mut tokens_in = 0;
    let mut tokens_out = 0;
    let mut duration_ms = 0;
    let mut stage_1b_final: Option<abstractive::Stage1bStats> = None;
    while let Some(evt) = stream.next().await {
        match evt? {
            AskEvent::Token(t) => answer.push_str(&t),
            AskEvent::Citation(c) => citations.push(c),
            AskEvent::HitInfo(h) => hits_used.push(h),
            AskEvent::Done {
                tokens_in: ti,
                tokens_out: to,
                degraded: d,
                duration_ms: ms,
                stage_1b: sb,
            } => {
                tokens_in = ti;
                tokens_out = to;
                degraded = d;
                duration_ms = ms;
                stage_1b_final = sb;
            }
            AskEvent::Error(e) => return Err(anyhow::anyhow!(e)),
        }
    }
    Ok(AskResponse {
        answer,
        citations,
        hits_used,
        degraded_to_mode_b: degraded,
        tokens_in,
        tokens_out,
        duration_ms,
        rewritten_question: match rewriter_status {
            session::RewriterStatus::Skipped => None,
            _ => Some(retrieval_query),
        },
        rewriter_status,
        stage_1b: stage_1b_final,
    })
}

async fn embed_query(q: &str) -> Result<Vec<f32>> {
    if let Some(mode) = super::ollama::mock_mode() {
        return Ok(super::ollama::mock_embed_vector(q, mode, 1024));
    }
    let cfg = crate::store::config::load_config().unwrap_or_default();
    let embed_cfg = crate::store::embedding::EmbeddingConfig::from_config(&cfg);
    crate::store::embedding::embed(q, &embed_cfg).await
}

fn hits_as_mode_b(hits: &[retrieve::ResolvedHit]) -> String {
    let mut out = String::from("[LLM unavailable] Here are the top relevant excerpts:\n\n");
    for (i, h) in hits.iter().enumerate().take(5) {
        out.push_str(&format!(
            "{}. [cit: {} {}/{}] — {}\n",
            i + 1,
            h.info.date,
            h.info.source,
            h.info.conv_id,
            h.snippet
        ));
    }
    out
}

fn citations_map(hits: &[retrieve::ResolvedHit]) -> std::collections::HashMap<String, Citation> {
    let mut m = std::collections::HashMap::new();
    for (i, h) in hits.iter().enumerate() {
        let anchor = prompt::cite_anchor(h);
        m.insert(
            anchor.clone(),
            Citation {
                id: (i + 1) as u32,
                date: h.info.date,
                source: h.info.source.clone(),
                conv_id: h.info.conv_id.clone(),
                line_hint: h.line_hint,
                span_index_in_summary: h.span_index_in_summary,
                snippet: h.snippet.clone(),
                score: h.info.score,
                compressed: h.compressed,
            },
        );
    }
    m
}

fn citations_fired_in(
    text: &str,
    map: &std::collections::HashMap<String, Citation>,
) -> Vec<Citation> {
    let mut out = Vec::new();
    for (anchor, cite) in map {
        if text.contains(anchor.as_str()) {
            out.push(cite.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_default_shape() {
        let f = Filters {
            source: vec![],
            since: None,
            until: None,
            min_score: 0.35,
        };
        assert_eq!(f.min_score, 0.35);
    }

    #[test]
    fn compression_enum_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Compression::Heuristic).unwrap(),
            "\"heuristic\""
        );
        assert_eq!(
            serde_json::to_string(&Compression::Abstractive).unwrap(),
            "\"abstractive\""
        );
    }

    #[test]
    fn citation_omits_compressed_field_when_none() {
        let c = Citation {
            id: 1,
            date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
            source: "cc".into(),
            conv_id: "c1".into(),
            line_hint: Some(1),
            span_index_in_summary: None,
            snippet: "s".into(),
            score: 0.9,
            compressed: None,
        };
        let j = serde_json::to_string(&c).unwrap();
        assert!(
            !j.contains("compressed"),
            "expected field omitted, got: {j}"
        );
    }

    #[test]
    fn citation_emits_compressed_field_when_set() {
        let c = Citation {
            id: 1,
            date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
            source: "cc".into(),
            conv_id: "c1".into(),
            line_hint: Some(1),
            span_index_in_summary: None,
            snippet: "s".into(),
            score: 0.9,
            compressed: Some(Compression::Abstractive),
        };
        let j = serde_json::to_string(&c).unwrap();
        assert!(j.contains("\"compressed\":\"abstractive\""), "got: {j}");
    }

    #[test]
    fn citation_deserializes_legacy_json_without_compressed_field() {
        // Backwards compat: TurnRecord.citations is serde-persisted as JSONL in
        // ask-session.jsonl across versions. A pre-3.5 record has no `compressed`
        // key — must still parse.
        let j = r#"{"id":1,"date":"2026-04-22","source":"cc","conv_id":"c1","line_hint":1,"span_index_in_summary":null,"snippet":"s","score":0.9}"#;
        let c: Citation = serde_json::from_str(j).expect("legacy Citation must parse");
        assert!(c.compressed.is_none());
    }

    #[test]
    fn ask_response_omits_stage_1b_when_none() {
        let r = AskResponse {
            answer: "".into(),
            citations: vec![],
            hits_used: vec![],
            degraded_to_mode_b: false,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
            rewritten_question: None,
            rewriter_status: session::RewriterStatus::Skipped,
            stage_1b: None,
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(!j.contains("stage_1b"), "expected field omitted, got: {j}");
    }

    #[test]
    fn ask_response_emits_stage_1b_when_set() {
        let r = AskResponse {
            answer: "".into(),
            citations: vec![],
            hits_used: vec![],
            degraded_to_mode_b: false,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
            rewritten_question: None,
            rewriter_status: session::RewriterStatus::Skipped,
            stage_1b: Some(abstractive::Stage1bStats {
                compressed_count: 2,
                cache_hits: 1,
                skipped_count: 0,
                duration_ms: 120,
            }),
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"stage_1b\""));
        assert!(j.contains("\"compressed_count\":2"));
        assert!(j.contains("\"cache_hits\":1"));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn ask_end_to_end_mock_empty_hits() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let req = AskRequest {
            question: "What did we do yesterday?".into(),
            filters: Filters {
                source: vec![],
                since: None,
                until: None,
                min_score: 0.35,
            },
            k_summary: 5,
            k_raw: 10,
            escalation_threshold: 0.5,
            mmr_threshold: 0.85,
            model: "qwen3:14b".into(),
            endpoint: "http://unused".into(),
            format: Format::Plain,
            max_context_tokens: 6000,
            response_tokens: 256,
            timeout: Duration::from_secs(5),
            no_escalate: false,
            debug_prompt: false,
            strict_citations: false,
            prior_turns: vec![],
            retrieval_query: "What did we do yesterday?".into(),
            rewriter_status: session::RewriterStatus::Skipped,
            compress_enabled: true,
            summarize_enabled: true,
            summarize_model: None,
            answer_backend: None,
            stage1b_backend: None,
        };
        // Empty index → should yield the "don't cover that" fallback.
        let resp = ask(req, Some(root)).await.unwrap();
        assert!(resp.answer.contains("don't cover that"));
        assert!(resp.citations.is_empty());
        assert!(!resp.degraded_to_mode_b);
        assert!(resp.rewritten_question.is_none());
        assert_eq!(resp.rewriter_status, session::RewriterStatus::Skipped);
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
