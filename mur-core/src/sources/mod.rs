//! External knowledge sources pipeline.
//!
//! A `KnowledgeSource` is a typed connection to a note app or RAG system. The
//! sync engine iterates documents, chunks them, embeds them, and writes to a
//! `VectorStore`. P1.1 defines the trait + registry skeleton only — adapters
//! arrive in P1.2 (Obsidian) and P1.4 (Notion, Joplin).

use anyhow::Result;
use async_trait::async_trait;

pub mod credentials;
pub mod instance;
pub mod kind;
pub mod types;

pub use kind::SourceKind;
pub use types::{Chunk, DocRef, Document, DocumentBody, SyncCursor};

/// Adapter interface. Implementors are stateless with respect to the
/// orchestrator; all cursor state is persisted in `SourceInstance`.
#[async_trait]
pub trait KnowledgeSource: Send + Sync {
    /// Stable id, e.g. `"notion:work"`.
    fn id(&self) -> &str;

    /// Behaviour kind.
    fn kind(&self) -> SourceKind;

    /// User-configurable multiplicative weight (from `SourceInstance`).
    fn weight(&self) -> f32;

    /// Incremental listing. `cursor == None` on first sync.
    async fn list_documents(&self, cursor: Option<SyncCursor>)
    -> Result<(Vec<DocRef>, SyncCursor)>;

    /// Fetch full content for one document.
    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document>;

    /// Adapter-specific chunking.
    fn chunk(&self, doc: &Document) -> Result<Vec<Chunk>>;

    /// External ids this adapter has deleted since `cursor`. The orchestrator
    /// always runs a set-diff fallback, so returning `Ok(vec![])` is safe.
    async fn list_deleted_since(&self, _cursor: Option<SyncCursor>) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

/// Closed-set registry. Phase 1 hardcodes the three adapter type names;
/// each adapter's factory function lives alongside the adapter.
pub const KNOWN_ADAPTER_TYPES: &[&str] = &["obsidian", "notion", "joplin"];

pub fn is_known_adapter_type(t: &str) -> bool {
    KNOWN_ADAPTER_TYPES.contains(&t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_adapter_types_sanity() {
        assert!(is_known_adapter_type("obsidian"));
        assert!(is_known_adapter_type("notion"));
        assert!(is_known_adapter_type("joplin"));
        assert!(!is_known_adapter_type("onenote"));
    }
}
