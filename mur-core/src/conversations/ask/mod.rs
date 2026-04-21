//! Mode C — Ask: local-only RAG with inline citations. See spec §5.
#![allow(dead_code)] // filled progressively across Tasks 19-25

use mur_common::Source;
use std::time::Duration;

pub mod cite;
pub mod format;
pub mod generate;
pub mod prompt;
pub mod retrieve;

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

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Citation {
    pub id: u32,
    pub date: chrono::NaiveDate,
    pub source: String, // file_prefix
    pub conv_id: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub snippet: String,
    pub score: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
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
    let query_embedding = match embed_query(&req.question).await {
        Ok(v) => v,
        Err(e) => {
            return Ok(Box::pin(try_stream! {
                yield AskEvent::Error(format!("embed failed: {e:#}"));
                yield AskEvent::Done {
                    tokens_in: 0,
                    tokens_out: 0,
                    degraded: false,
                    duration_ms: start.elapsed().as_millis() as u64,
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
            };
        }));
    }

    // 3. Build prompt
    let prompt = prompt::render(
        &req.question,
        &hits,
        req.max_context_tokens,
        req.response_tokens,
    );

    let hit_events: Vec<AskEvent> = hits
        .iter()
        .map(|h| AskEvent::HitInfo(h.info.clone()))
        .collect();

    // 4. Generate (streaming) with grounding filter
    let endpoint = req.endpoint.clone();
    let model = req.model.clone();
    let filter = if req.strict_citations {
        cite::GroundingFilter::new_strict(prompt.valid_citations.clone())
    } else {
        cite::GroundingFilter::new(prompt.valid_citations.clone())
    };
    let tokens_in = prompt.tokens_est;

    let stream = match generate::stream_answer(
        &endpoint,
        &model,
        &prompt.system,
        &prompt.user,
        req.response_tokens as u32,
        req.timeout,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            let mode_b = hits_as_mode_b(&hits);
            return Ok(Box::pin(try_stream! {
                for evt in hit_events { yield evt; }
                yield AskEvent::Token(mode_b);
                yield AskEvent::Done {
                    tokens_in,
                    tokens_out: 0,
                    degraded: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                };
                yield AskEvent::Error(format!("ollama unavailable: {e:#}"));
            }));
        }
    };

    let citation_events_by_anchor = citations_map(&hits);
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
        };
    };
    Ok(Box::pin(out_stream))
}

pub async fn ask(req: AskRequest, root_override: Option<&str>) -> Result<AskResponse> {
    let mut stream = ask_stream(req, root_override).await?;
    let mut answer = String::new();
    let mut citations = Vec::new();
    let mut hits_used = Vec::new();
    let mut degraded = false;
    let mut tokens_in = 0;
    let mut tokens_out = 0;
    let mut duration_ms = 0;
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
            } => {
                tokens_in = ti;
                tokens_out = to;
                degraded = d;
                duration_ms = ms;
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
        };
        // Empty index → should yield the "don't cover that" fallback.
        let resp = ask(req, Some(root)).await.unwrap();
        assert!(resp.answer.contains("don't cover that"));
        assert!(resp.citations.is_empty());
        assert!(!resp.degraded_to_mode_b);
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
