//! Abstract vector store trait.
//!
//! Phase 1 has one implementation (`lancedb::LanceDbStore`). Phase 1.3 adds
//! `qdrant::QdrantStore`. Callers interact through `Arc<dyn VectorStore>`
//! obtained from `factory::get_vector_store(&config, &index_dir)`.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

pub mod factory;
pub mod lancedb;

#[cfg(test)]
pub mod tests;

pub use self::lancedb::LanceDbStore;

/// An embedded chunk ready to be stored.
///
/// This shape is re-used by the `sources` pipeline (external notes) and the
/// pattern index. For P1.1 we only use it for sources — the pattern path
/// keeps its existing `build_unified_index` code path untouched.
#[derive(Debug, Clone)]
pub struct EmbeddedChunk {
    pub chunk_id: String,
    pub source_id: String,
    pub external_id: String,
    pub ordinal: usize,
    pub text: String,
    pub heading_path: Vec<String>,
    pub char_range: (usize, usize),
    pub updated_at: DateTime<Utc>,
    pub embedding: Vec<f32>,
}

/// Filter applied at search time.
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    /// Restrict to these source_ids. `None` = all sources.
    pub source_ids: Option<Vec<String>>,
    /// Only return rows whose `updated_at` is at least this timestamp.
    pub since: Option<DateTime<Utc>>,
}

/// A single search hit from the new trait-based API.
#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk_id: String,
    pub source_id: String,
    pub external_id: String,
    /// Similarity score in [0.0, 1.0], higher = better.
    pub score: f32,
    pub text: String,
    pub heading_path: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

/// Abstract vector store. Implementations MUST be `Send + Sync` and idempotent for `upsert`.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Insert or replace chunks by `chunk_id`.
    async fn upsert(&self, chunks: &[EmbeddedChunk]) -> Result<()>;

    /// Search by embedding.
    async fn search(
        &self,
        query_vec: &[f32],
        k: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<Hit>>;

    /// Delete chunks whose (source_id, external_id) matches.
    async fn delete_by_external_ids(
        &self,
        source_id: &str,
        external_ids: &[String],
    ) -> Result<()>;

    /// Delete all chunks for a given source.
    async fn delete_by_source(&self, source_id: &str) -> Result<()>;

    /// List every `external_id` currently indexed for `source_id`.
    async fn list_external_ids(&self, source_id: &str) -> Result<Vec<String>>;

    /// Count chunks (optionally per source).
    async fn count(&self, source_id: Option<&str>) -> Result<usize>;

    /// Drop and recreate all state.
    async fn rebuild_index(&self) -> Result<()>;
}
