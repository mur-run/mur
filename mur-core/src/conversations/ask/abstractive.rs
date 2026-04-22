//! Phase 3.5 Stage 1b — LLM-abstractive hit compression.
//!
//! Sits between Phase 3.4's heuristic Stage 1 and the existing Stage 2
//! (drop-oldest-history) in `prompt::render`'s overflow cascade. Per-hit,
//! largest-first, sequential; every call is wrapped in a 5-second timeout
//! and soft-fails (warn + keep original). Results cache to
//! `~/.mur/conversations/cache/abstractive/<sha256>.txt`.
//!
//! See `docs/superpowers/specs/2026-04-22-mur-conversations-phase-3-5-design.md`.

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
#[derive(Debug)]
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

/// Orchestrate Stage 1b: sort hits largest-first, sequentially compress
/// until the token overshoot is resolved or candidates are exhausted.
///
/// `cur_tokens` and `max_context_tokens` are caller-measured in _tokens_ (not
/// chars) but the per-hit char → token conversion uses the same `len / 4`
/// heuristic `prompt::tokens_est` uses, so they're proportional. Re-measuring
/// between hits happens by the caller after this function returns — this fn
/// operates on pre-computed overshoot to avoid owning the `prompt::render`
/// responsibility of re-building the full prompt string.
///
/// Invariants:
/// - Sorted by `hit.snippet.len()` descending at entry — stable sort by
///   original index on ties so deterministic.
/// - Early-exits when estimated post-compression tokens ≤ max.
/// - `target_tokens_per_hit` floored at `MIN_TARGET_TOKENS_PER_HIT`.
pub async fn run_stage_1b(
    ctx: &AbstractiveCtx<'_>,
    hits: &mut [ResolvedHit],
    cur_tokens: usize,
    max_context_tokens: usize,
) -> Stage1bSummary {
    let start = std::time::Instant::now();
    let mut summary = Stage1bSummary {
        processed: 0,
        compressed_count: 0,
        cache_hits: 0,
        skipped: Vec::new(),
        duration_ms: 0,
    };
    if cur_tokens <= max_context_tokens {
        summary.duration_ms = start.elapsed().as_millis() as u64;
        return summary;
    }

    // Index list sorted largest-first (by snippet byte length). Keep indices so
    // we can mutate the original slice in place via &mut hits[idx].
    let mut order: Vec<usize> = (0..hits.len()).collect();
    order.sort_by(|&a, &b| {
        hits[b]
            .snippet
            .len()
            .cmp(&hits[a].snippet.len())
            .then(a.cmp(&b))
    });

    let mut cur_tokens = cur_tokens;
    let total = order.len();

    for (k, idx) in order.into_iter().enumerate() {
        if cur_tokens <= max_context_tokens {
            break;
        }
        let overshoot = cur_tokens.saturating_sub(max_context_tokens);
        let rem_denom = total - k; // always ≥ 1 (k < total inside the loop)
        // ceil-div for "share out" the overshoot reduction.
        let reduce_by = overshoot.div_ceil(rem_denom);
        let cur_hit_tokens = hits[idx].snippet.len() / 4;
        let target = cur_hit_tokens
            .saturating_sub(reduce_by)
            .max(MIN_TARGET_TOKENS_PER_HIT);

        let before_tokens = cur_hit_tokens;
        let outcome = compress_hit(ctx, &mut hits[idx], target).await;
        summary.processed += 1;
        match outcome {
            CompressOutcome::Compressed => summary.compressed_count += 1,
            CompressOutcome::CacheHit => summary.cache_hits += 1,
            CompressOutcome::Skipped(reason) => summary.skipped.push((idx, reason)),
        }
        let after_tokens = hits[idx].snippet.len() / 4;
        let delta = before_tokens.saturating_sub(after_tokens);
        cur_tokens = cur_tokens.saturating_sub(delta);
    }

    summary.duration_ms = start.elapsed().as_millis() as u64;
    summary
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

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn run_stage_1b_early_exits_when_fit_after_two_hits() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        // 5 hits; budget just tight enough that 1-2 compressions fit it (exact count
        // depends on the mock response size). The assertion only requires 1 ≤ processed < 5.
        let mut hits: Vec<ResolvedHit> = (0..5).map(|_| long_hit(20)).collect();
        let orig_total: usize = hits.iter().map(|h| h.snippet.len()).sum();
        let max_context_chars = orig_total - 200; // force overflow on char-ish metric
        let summary = run_stage_1b(
            &ctx(&client, tmp.path().to_str().unwrap()),
            &mut hits,
            orig_total,
            max_context_chars,
        )
        .await;
        assert!(
            summary.processed >= 1,
            "at least one hit must be touched; got {}",
            summary.processed
        );
        assert!(
            summary.processed < 5,
            "early-exit should prevent touching all 5; got {}",
            summary.processed
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn run_stage_1b_largest_first_order() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        unsafe { std::env::remove_var("MUR_ABSTRACTIVE_MOCK_FAIL") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let mut hits = vec![long_hit(5), long_hit(30), long_hit(10)];
        let big_idx_before = 1; // middle hit is the largest
        let orig_big_len = hits[big_idx_before].snippet.len();
        let orig_total: usize = hits.iter().map(|h| h.snippet.len()).sum();
        // Budget forces at least one compression; largest hit must be the one touched
        // first regardless of total count.
        let max_context_chars = orig_total - (orig_big_len / 3);
        let summary = run_stage_1b(
            &ctx(&client, tmp.path().to_str().unwrap()),
            &mut hits,
            orig_total,
            max_context_chars,
        )
        .await;
        assert!(summary.compressed_count >= 1);
        // The largest hit should have shrunk.
        assert!(
            hits[big_idx_before].snippet.len() < orig_big_len,
            "largest-first: biggest hit should be touched first"
        );
        assert_eq!(
            hits[big_idx_before].compressed,
            Some(super::super::Compression::Abstractive)
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn run_stage_1b_noop_when_already_fits() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let tmp = tempfile::tempdir().unwrap();
        let mut hits = vec![long_hit(5)];
        let orig_total: usize = hits.iter().map(|h| h.snippet.len()).sum();
        let summary = run_stage_1b(
            &ctx(&client, tmp.path().to_str().unwrap()),
            &mut hits,
            orig_total,
            orig_total + 10_000, // huge budget → no overshoot
        )
        .await;
        assert_eq!(summary.processed, 0);
        assert_eq!(summary.compressed_count, 0);
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
