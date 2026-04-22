//! Phase 3.5 Stage 1b — LLM-abstractive hit compression.
//!
//! Sits between Phase 3.4's heuristic Stage 1 and the existing Stage 2
//! (drop-oldest-history) in `prompt::render`'s overflow cascade. Per-hit,
//! largest-first, sequential; every call is wrapped in a 5-second timeout
//! and soft-fails (warn + keep original). Results cache to
//! `~/.mur/conversations/cache/abstractive/<sha256>.txt`.
//!
//! See `docs/superpowers/specs/2026-04-22-mur-conversations-phase-3-5-design.md`.
#![allow(dead_code)] // wired by Task 8 (prompt::render integration).

use super::cache;
use super::retrieve::ResolvedHit;
use crate::conversations::ollama::{GenerateOptions, GenerateRequest, OllamaClient};
use std::time::Duration;

/// Prompt-version marker baked into cache keys. Bump when the prompt template
/// or validator changes — existing cached entries become natural misses
/// rather than needing a sweep.
pub const PROMPT_VERSION: &str = "mur-abstract-v1";

/// Fixed per-call timeout. Hardcoded by design (see spec §2 non-goals).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Floor for `target_tokens_per_hit` — never ask the LLM to squeeze below
/// ~60 tokens (prevents degenerate 1-word summaries).
pub const MIN_TARGET_TOKENS_PER_HIT: usize = 60;

/// Minimum content length before Stage 1b considers a hit. Mirrors
/// `compress::COMPRESS_MIN_CHARS` — no LLM call is worth it for < 400 chars.
pub const MIN_CONTENT_CHARS: usize = 400;

const SYSTEM_TEMPLATE: &str = "You compress text for retrieval context. Preserve entities, \
dates, numbers, and decisions. Do not add facts. Output only the summary — no preamble, \
no markdown.";

fn user_template(target_tokens: usize, content: &str) -> String {
    format!("Summarize the following in ≤{target_tokens} tokens.\n\n{content}")
}

/// Per-run Stage 1b context. Built once in `ask_stream` from `AskConfig`.
pub struct AbstractiveCtx<'a> {
    pub client: &'a OllamaClient,
    pub model: &'a str,
    pub timeout: Duration,
    pub root_override: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressOutcome {
    /// Fresh compression — Ollama call succeeded, cache was written.
    Compressed,
    /// Cache short-circuited — no LLM call made.
    CacheHit,
    /// Soft-fail — reason tag. See `skip_reason::*` constants.
    Skipped(&'static str),
}

pub mod skip_reason {
    pub const TIMEOUT: &str = "timeout";
    pub const EMPTY: &str = "empty";
    pub const NOT_SHORTER: &str = "not_shorter";
    pub const OLLAMA_ERR: &str = "ollama_err";
    pub const TOO_SHORT: &str = "too_short";
}

/// Aggregated stats from one `run_stage_1b` invocation. Drives log lines
/// and JSON output. `skipped` is per-hit detail for `tracing::warn!`.
pub struct Stage1bSummary {
    pub processed: usize,
    pub compressed_count: usize,
    pub cache_hits: usize,
    pub skipped: Vec<(usize, &'static str)>,
    pub duration_ms: u64,
}

/// Serializable slim projection for `AskResponse.stage_1b`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Stage1bStats {
    pub compressed_count: usize,
    pub cache_hits: usize,
    pub skipped_count: usize,
    pub duration_ms: u64,
}

impl Stage1bSummary {
    pub fn to_stats(&self) -> Stage1bStats {
        Stage1bStats {
            compressed_count: self.compressed_count,
            cache_hits: self.cache_hits,
            skipped_count: self.skipped.len(),
            duration_ms: self.duration_ms,
        }
    }
}

/// Compress one hit. Soft-fails on every error path; never bubbles up.
///
/// Algorithm (spec §5):
/// 1. Short-circuit if content < `MIN_CONTENT_CHARS` → `Skipped(TOO_SHORT)`.
/// 2. Compute cache key. If hit, load and apply. Return `CacheHit`.
/// 3. Call Ollama wrapped in `tokio::time::timeout(ctx.timeout)`.
/// 4. Validate: non-empty after trim, strictly shorter than original.
/// 5. On success: write cache, mutate `hit.snippet`, tag `compressed =
///    Some(Abstractive)`, return `Compressed`.
pub async fn compress_hit(
    ctx: &AbstractiveCtx<'_>,
    hit: &mut ResolvedHit,
    target_tokens: usize,
) -> CompressOutcome {
    if hit.snippet.len() < MIN_CONTENT_CHARS {
        return CompressOutcome::Skipped(skip_reason::TOO_SHORT);
    }
    let target = target_tokens.max(MIN_TARGET_TOKENS_PER_HIT);
    let key = cache::cache_key(ctx.model, target, &hit.snippet);

    if let Some(cached) = cache::cache_get(&key, ctx.root_override) {
        if !cached.is_empty() && cached.len() < hit.snippet.len() {
            hit.snippet = cached;
            hit.compressed = Some(super::Compression::Abstractive);
            return CompressOutcome::CacheHit;
        }
        // Cached value invalid (empty, or unexpectedly not-shorter because
        // hit content drifted) — fall through and try a fresh call.
        tracing::debug!(
            key,
            cached_len = cached.len(),
            orig_len = hit.snippet.len(),
            "cache entry present but invalid, retrying"
        );
    }

    let prompt = user_template(target, &hit.snippet);
    let req = GenerateRequest {
        model: ctx.model,
        prompt: &prompt,
        system: Some(SYSTEM_TEMPLATE),
        stream: false,
        options: GenerateOptions {
            temperature: Some(0.0),
            top_p: None,
            num_predict: Some(target as u32 * 2),
            stop: Vec::new(),
        },
    };

    let call = ctx.client.generate(req);
    let out = match tokio::time::timeout(ctx.timeout, call).await {
        Err(_) => {
            tracing::warn!(target, len = hit.snippet.len(), "stage-1b timeout");
            return CompressOutcome::Skipped(skip_reason::TIMEOUT);
        }
        Ok(Err(e)) => {
            tracing::warn!(target, err = ?e, "stage-1b ollama error");
            return CompressOutcome::Skipped(skip_reason::OLLAMA_ERR);
        }
        Ok(Ok(resp)) => resp.response,
    };

    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        return CompressOutcome::Skipped(skip_reason::EMPTY);
    }
    if trimmed.len() >= hit.snippet.len() {
        return CompressOutcome::Skipped(skip_reason::NOT_SHORTER);
    }

    if let Err(e) = cache::cache_put(&key, &trimmed, ctx.root_override) {
        // Cache write failure is non-fatal — still apply the summary.
        tracing::warn!(key, err = ?e, "stage-1b cache write failed");
    }

    hit.snippet = trimmed;
    hit.compressed = Some(super::Compression::Abstractive);
    CompressOutcome::Compressed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::ENV_LOCK;
    use crate::conversations::ask::HitInfo;
    use std::time::Duration;

    fn long_hit(n_sentences: usize) -> ResolvedHit {
        let snippet = (0..n_sentences)
            .map(|i| format!("Hit body fact {i} with some supporting narrative text."))
            .collect::<Vec<_>>()
            .join(" ");
        ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "c1".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                score: 0.9,
            },
            snippet,
            line_hint: Some(1),
            span_index_in_summary: None,
            vector: None,
            compressed: None,
        }
    }

    fn ctx<'a>(client: &'a OllamaClient, root: &'a str) -> AbstractiveCtx<'a> {
        AbstractiveCtx {
            client,
            model: "qwen3:14b",
            timeout: Duration::from_millis(200),
            root_override: Some(root),
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_skips_when_content_too_short() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let mut h = long_hit(1);
        h.snippet = "tiny".into();
        let o = compress_hit(&ctx(&client, tmp.path().to_str().unwrap()), &mut h, 128).await;
        assert_eq!(o, CompressOutcome::Skipped(skip_reason::TOO_SHORT));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_success_shortens_snippet_and_writes_cache() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut h = long_hit(20); // ~1100 chars, way over MIN_CONTENT_CHARS
        let orig_len = h.snippet.len();
        let o = compress_hit(&ctx(&client, root), &mut h, 128).await;
        assert_eq!(o, CompressOutcome::Compressed);
        assert!(h.snippet.len() < orig_len, "mock summary must be shorter");
        assert_eq!(h.compressed, Some(super::super::Compression::Abstractive));
        // Cache entry should exist.
        let key = cache::cache_key("qwen3:14b", 128, &long_hit(20).snippet);
        let cached = cache::cache_get(&key, Some(root));
        assert_eq!(cached.as_deref(), Some(h.snippet.as_str()));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_cache_hit_on_second_call_skips_llm() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut h1 = long_hit(20);
        compress_hit(&ctx(&client, root), &mut h1, 128).await;
        let mut h2 = long_hit(20);
        let o = compress_hit(&ctx(&client, root), &mut h2, 128).await;
        assert_eq!(o, CompressOutcome::CacheHit);
        assert_eq!(h2.snippet, h1.snippet);
        assert_eq!(h2.compressed, Some(super::super::Compression::Abstractive));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[ignore = "mock FAIL hook lands in Task 7"]
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_respects_timeout() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::set_var("MUR_ABSTRACTIVE_MOCK_FAIL", "timeout") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(30));
        let tmp = tempfile::tempdir().unwrap();
        let mut h = long_hit(20);
        let o = compress_hit(
            &AbstractiveCtx {
                client: &client,
                model: "qwen3:14b",
                timeout: Duration::from_millis(100),
                root_override: Some(tmp.path().to_str().unwrap()),
            },
            &mut h,
            128,
        )
        .await;
        assert_eq!(o, CompressOutcome::Skipped(skip_reason::TIMEOUT));
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[ignore = "mock FAIL hook lands in Task 7"]
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_skips_on_empty_response() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::set_var("MUR_ABSTRACTIVE_MOCK_FAIL", "empty") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let mut h = long_hit(20);
        let o = compress_hit(&ctx(&client, tmp.path().to_str().unwrap()), &mut h, 128).await;
        assert_eq!(o, CompressOutcome::Skipped(skip_reason::EMPTY));
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[ignore = "mock FAIL hook lands in Task 7"]
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn compress_hit_skips_when_not_shorter() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::set_var("MUR_ABSTRACTIVE_MOCK_FAIL", "not_shorter") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let mut h = long_hit(20);
        let o = compress_hit(&ctx(&client, tmp.path().to_str().unwrap()), &mut h, 128).await;
        assert_eq!(o, CompressOutcome::Skipped(skip_reason::NOT_SHORTER));
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
