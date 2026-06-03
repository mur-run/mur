pub mod bm25;
pub mod ccr;
pub mod compressors;
pub mod config;
pub mod detect;
pub mod stats;
pub mod tokenizer;
pub mod types;

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
        Ok(Self { store, tok: default_counter(), config, stats })
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

    /// Compress `content`. Never errors: any failure returns the original.
    pub fn compress(&self, content: &str, query: Option<&str>) -> CompressResult {
        let ct = detect_content_type(content, &self.config);
        let ctx = CompressCtx { query, config: &self.config };
        let out = self.dispatch(ct, content, &ctx).unwrap_or_else(|_| CompressOutput {
            compressed: content.to_string(),
            hash: None,
            transforms: Vec::new(),
        });

        let before = self.tok.count(content);
        let after = self.tok.count(&out.compressed);
        let saved = before.saturating_sub(after);
        let pct = if before > 0 { saved as f32 / before as f32 * 100.0 } else { 0.0 };
        self.stats.record_compression(before, after);

        CompressResult {
            compressed: out.compressed,
            hash: out.hash,
            original_tokens: before,
            compressed_tokens: after,
            tokens_saved: saved,
            savings_percent: pct,
            transforms: out.transforms,
            content_type: ct,
        }
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
                let ranked = bm25_rank(q, &entry.items);
                let max = ranked.first().map(|(_, s)| *s).unwrap_or(1.0).max(1e-6);
                let results: Vec<String> = ranked
                    .into_iter()
                    .map(|(i, s)| (i, s / max))
                    .filter(|(_, s)| *s >= self.config.retrieve_score_threshold)
                    .take(self.config.retrieve_top_k)
                    .map(|(i, _)| entry.items[i].clone())
                    .collect();
                RetrieveResult::Filtered { query: q.to_string(), count: results.len(), results }
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
        self.stats.snapshot(self.config.stats.cost_per_mtok_usd, entries, bytes)
    }
}
