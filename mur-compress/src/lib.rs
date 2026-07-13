//! Design inspiration: headroom (https://github.com/chopratejas/headroom, Apache-2.0).
//! Clean-room reimplementation — no headroom source is copied.

pub mod auto;
pub mod bm25;
pub mod ccr;
pub mod compressors;
pub mod config;
pub mod detect;
pub mod skeleton;
pub mod stats;
pub mod tokenizer;
pub mod types;

pub use auto::{
    AutoCfg, AutoOutcome, auto_compress, auto_compress_value, retrieval_envelope, retrieval_note,
};
pub use bm25::bm25_rank;
pub use ccr::{CcrStore, CompressedEntry};
pub use config::CompressConfig;
pub use detect::detect_content_type;
pub use stats::{StatsSnapshot, StatsTracker};
pub use tokenizer::{TokenCounter, default_counter};
pub use types::{
    CompressCtx, CompressError, CompressOutput, CompressResult, ContentType, RetrieveResult,
};

use std::path::PathBuf;

use crate::compressors::{diff, fallback, json, log, search};
// IMPORTANT: do NOT add `use crate::ccr::CcrStore;`, `use crate::config::CompressConfig;`,
// `use crate::tokenizer::{default_counter, TokenCounter};`, etc. Those names are already
// in crate-root scope via the `pub use` re-exports above. Re-`use`-ing them here causes
// "the name `X` is defined multiple times" compile errors.

/// Top-level engine: owns the store, tokenizer, config, and stats.
pub struct CompressEngine {
    store: CcrStore,
    tok: Box<dyn TokenCounter>,
    config: CompressConfig,
    stats: StatsTracker,
}

impl CompressEngine {
    pub fn new(dir: impl Into<PathBuf>, config: CompressConfig) -> std::io::Result<Self> {
        let dir = dir.into();
        let store = CcrStore::new(
            &dir,
            config.ttl_secs(),
            config.store.max_entries,
            config.store.max_bytes,
            config.store.compress_at_rest,
        )?;
        let stats = StatsTracker::new(dir.join("stats.json"));
        Ok(Self {
            store,
            tok: default_counter(),
            config,
            stats,
        })
    }

    /// Token count for `content` using the engine's configured tokenizer.
    pub fn count_tokens(&self, content: &str) -> usize {
        self.tok.count(content)
    }

    /// Read-only access to the engine's configuration (e.g. the `auto` gates).
    pub fn config(&self) -> &CompressConfig {
        &self.config
    }

    fn dispatch(
        &self,
        ct: ContentType,
        content: &str,
        ctx: &CompressCtx,
    ) -> Result<CompressOutput, CompressError> {
        match ct {
            ContentType::SearchResults => {
                search::compress(content, ctx, &self.store, self.tok.as_ref())
            }
            ContentType::BuildLog => log::compress(content, ctx, &self.store, self.tok.as_ref()),
            ContentType::GitDiff => diff::compress(content, ctx, &self.store, self.tok.as_ref()),
            ContentType::Json => json::compress(content, ctx, &self.store, self.tok.as_ref()),
            ContentType::Generic => {
                fallback::compress(content, ctx, &self.store, self.tok.as_ref())
            }
        }
    }

    /// A no-op result that returns `content` unchanged.
    fn passthrough(content: &str, before: usize, ct: ContentType) -> CompressResult {
        CompressResult {
            compressed: content.to_string(),
            hash: None,
            original_tokens: before,
            compressed_tokens: before,
            tokens_saved: 0,
            savings_percent: 0.0,
            transforms: vec!["passthrough".to_string()],
            content_type: ct,
        }
    }

    /// Compress `content`. Never errors: any failure returns the original.
    ///
    /// The result is only kept if it actually pays off — it must be strictly
    /// smaller than the input, and any result that offloaded to the store must
    /// clear `bloat_threshold`. Otherwise the original is returned unchanged so
    /// compression can never "help backwards". Disabled config is also a no-op.
    pub fn compress(&self, content: &str, query: Option<&str>) -> CompressResult {
        let ct = detect_content_type(content, &self.config);
        let before = self.tok.count(content);

        if !self.config.enabled {
            return Self::passthrough(content, before, ct);
        }

        // Generic early-skip: the fallback's savings are exactly computable
        // in one scan. Below the configured floor, don't run the compressor
        // at all — record a skip (not a compression) and pass through.
        if ct == ContentType::Generic
            && fallback::saving_ratio(content) < self.config.fallback.min_save_ratio
        {
            self.stats.record_skip(ct.as_str());
            let mut r = Self::passthrough(content, before, ct);
            r.transforms = vec!["skipped".to_string()];
            return r;
        }

        let ctx = CompressCtx {
            query,
            config: &self.config,
        };
        let out = self
            .dispatch(ct, content, &ctx)
            .unwrap_or_else(|_| CompressOutput {
                compressed: content.to_string(),
                hash: None,
                transforms: Vec::new(),
            });

        let after = self.tok.count(&out.compressed);
        let saved = before.saturating_sub(after);
        let saved_frac = if before > 0 {
            saved as f32 / before as f32
        } else {
            0.0
        };

        // Reject results that don't pay off. Reformat-only output (no hash) just
        // has to be smaller; offloaded output (hash present, paid a store write)
        // must also clear the bloat gate. A rejected offload leaves an orphan
        // store entry that ages out via TTL/eviction — a future estimate_bloat()
        // pre-gate could avoid the wasted write entirely.
        let worth_it =
            after < before && (out.hash.is_none() || saved_frac >= self.config.bloat_threshold);
        if !worth_it {
            return Self::passthrough(content, before, ct);
        }

        self.stats.record_compression(ct.as_str(), before, after);

        CompressResult {
            compressed: out.compressed,
            hash: out.hash,
            original_tokens: before,
            compressed_tokens: after,
            tokens_saved: saved,
            savings_percent: saved_frac * 100.0,
            transforms: out.transforms,
            content_type: ct,
        }
    }

    /// Unconditionally store `content` in the CCR store and return its hash,
    /// bypassing `compress()`'s content-type dispatch and payoff gating.
    ///
    /// `compress()` only offloads when the matched content-type compressor
    /// chooses to (`fallback`/Generic — most prose and code — never does).
    /// Callers that need a retrieval handle for content they are dropping for
    /// reasons *other* than "it compresses well" (e.g. a superseded read, an
    /// exact duplicate of a later tool result) use this instead, so the
    /// original stays recoverable via [`Self::retrieve`] regardless of
    /// content type. Best-effort: a store write failure returns `None`, and
    /// the caller falls back to a non-retrievable stub.
    pub fn archive(&self, content: &str) -> Option<String> {
        self.store
            .put_original(content, Vec::new(), ContentType::Generic, self.tok.as_ref())
            .ok()
    }

    /// Retrieve a stored original by hash, optionally BM25-filtered by query.
    pub fn retrieve(&self, hash: &str, query: Option<&str>) -> RetrieveResult {
        let entry = match self.store.get(hash) {
            Ok(Some(e)) => e,
            _ => return RetrieveResult::NotFound,
        };
        self.stats.record_retrieval();
        match query {
            Some(q) => {
                // `retrieve_score_threshold` filters on score *relative to the
                // best match* (scores are max-normalized below), not an absolute
                // BM25 value — raw BM25 is unbounded and corpus-dependent. The
                // top hit always normalizes to 1.0, so a query that matches at
                // all returns at least its best item; the threshold prunes the
                // long tail of weakly-related items.
                let ranked = bm25_rank(q, &entry.items);
                let max = ranked.first().map(|(_, s)| *s).unwrap_or(1.0).max(1e-6);
                let results: Vec<String> = ranked
                    .into_iter()
                    .map(|(i, s)| (i, s / max))
                    .filter(|(_, s)| *s >= self.config.retrieve_score_threshold)
                    .take(self.config.retrieve_top_k)
                    .map(|(i, _)| entry.items[i].clone())
                    .collect();
                RetrieveResult::Filtered {
                    query: q.to_string(),
                    count: results.len(),
                    results,
                }
            }
            None => RetrieveResult::Full {
                content_type: entry.content_type,
                original_content: entry.original_text,
                item_count: entry.item_count,
            },
        }
    }

    pub fn stats_snapshot(&self) -> StatsSnapshot {
        let (entries, bytes) = self.store.stats();
        self.stats
            .snapshot(self.config.stats.cost_per_mtok_usd, entries, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(dir: &std::path::Path, cfg: CompressConfig) -> CompressEngine {
        CompressEngine::new(dir, cfg).unwrap()
    }

    #[test]
    fn archive_stores_generic_content_compress_would_skip() {
        let dir = tempfile::tempdir().unwrap();
        let eng = engine(dir.path(), CompressConfig::default());
        let prose = "fn main() {\n    println!(\"hi\");\n}\n".repeat(50);
        // compress() on plain code/prose (Generic) never offloads.
        assert!(eng.compress(&prose, None).hash.is_none());
        let hash = eng.archive(&prose).expect("archive should store");
        match eng.retrieve(&hash, None) {
            RetrieveResult::Full {
                original_content, ..
            } => {
                assert_eq!(original_content, prose);
            }
            other => panic!("expected Full, got {other:?}"),
        }
    }

    #[test]
    fn disabled_config_is_a_passthrough() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = CompressConfig {
            enabled: false,
            ..Default::default()
        };
        let eng = engine(dir.path(), cfg);
        // A long search result that would normally offload.
        let input = (0..40)
            .map(|i| format!("src/f{i}.rs:{i}:line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let res = eng.compress(&input, Some("line 7"));
        assert!(res.hash.is_none(), "disabled engine must not offload");
        assert_eq!(
            res.compressed, input,
            "disabled engine returns input verbatim"
        );
        assert_eq!(res.tokens_saved, 0);
        assert_eq!(
            eng.stats_snapshot().compressions,
            0,
            "no-op is not recorded"
        );
    }

    #[test]
    fn expanding_result_passes_through_instead_of_helping_backwards() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = CompressConfig::default();
        cfg.store.compress_at_rest = false;
        cfg.protect_head_lines = 2;
        let eng = engine(dir.path(), cfg);
        // A tiny array just over the collapse threshold: the schema+sample+hash
        // wrapper is larger than the original, so it must NOT be kept.
        let input = r#"[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5},{"id":6}]"#;
        let res = eng.compress(input, None);
        assert!(
            res.hash.is_none(),
            "expanding compression must pass through"
        );
        assert_eq!(res.compressed, input);
        assert_eq!(res.tokens_saved, 0);
        assert!(res.compressed_tokens <= res.original_tokens);
        assert_eq!(eng.stats_snapshot().compressions, 0);
    }

    #[test]
    fn generic_low_savings_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CompressEngine::new(dir.path(), CompressConfig::default()).unwrap();
        // Prose with nothing to strip: predicted savings 0 < 5% → skip.
        let content = "plain prose line\n".repeat(200);
        let r = engine.compress(&content, None);
        assert_eq!(r.tokens_saved, 0);
        assert!(r.transforms.iter().any(|t| t == "skipped"));
        let snap = engine.stats_snapshot();
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.compressions, 0);
    }

    #[test]
    fn generic_high_savings_still_compresses() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CompressEngine::new(dir.path(), CompressConfig::default()).unwrap();
        // Heavy trailing whitespace: well above 5% → real compression.
        let content = "x                                                            \n".repeat(300);
        let r = engine.compress(&content, None);
        assert!(r.tokens_saved > 0);
        assert!(r.transforms.iter().any(|t| t == "fallback.whitespace"));
    }
}
