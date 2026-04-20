//! Core types passed through the sources pipeline.
//!
//! These live in `mur-core` (Phase 1). If a future mur-server integration
//! needs them, hoist to `mur-common` then.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque cursor returned by `KnowledgeSource::list_documents`. Each adapter
/// defines its own encoding (e.g. Notion: RFC3339 timestamp, Joplin: epoch-ms).
/// Orchestrator stores it verbatim in the source yaml and passes back next sync.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncCursor(pub String);

impl SyncCursor {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Lightweight reference to a document that hasn't been fetched yet.
#[derive(Debug, Clone)]
pub struct DocRef {
    pub external_id: String,
    pub title: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// A full document payload.
#[derive(Debug, Clone)]
pub struct Document {
    pub source_id: String,
    pub external_id: String,
    pub title: String,
    pub body: DocumentBody,
    pub url: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub metadata: serde_json::Value,
}

/// Body form; adapters pick the variant that preserves the most fidelity.
#[derive(Debug, Clone)]
pub enum DocumentBody {
    Markdown(String),
    PlainText(String),
    /// Notion blocks — serialized as opaque JSON so we don't depend on the
    /// Notion SDK crate from `types.rs`.
    NotionBlocks(serde_json::Value),
}

impl DocumentBody {
    /// Returns the content as plaintext suitable for embedding.
    pub fn as_plain_text(&self) -> String {
        match self {
            DocumentBody::Markdown(s) | DocumentBody::PlainText(s) => s.clone(),
            DocumentBody::NotionBlocks(_) => {
                // Real extraction lives in the Notion chunker (P1.4). For P1.1
                // we expose an empty fallback — no adapter produces this variant yet.
                String::new()
            }
        }
    }
}

/// Pre-embedding chunk.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub chunk_id: String,
    pub source_id: String,
    pub external_id: String,
    pub ordinal: usize,
    pub text: String,
    pub heading_path: Vec<String>,
    pub char_range: (usize, usize),
    pub updated_at: DateTime<Utc>,
}

impl Chunk {
    /// Build a chunk with a fresh UUID v4 chunk_id.
    pub fn new(
        source_id: impl Into<String>,
        external_id: impl Into<String>,
        ordinal: usize,
        text: impl Into<String>,
        heading_path: Vec<String>,
        char_range: (usize, usize),
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            chunk_id: Uuid::new_v4().to_string(),
            source_id: source_id.into(),
            external_id: external_id.into(),
            ordinal,
            text: text.into(),
            heading_path,
            char_range,
            updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_cursor_default_is_empty() {
        assert!(SyncCursor::default().is_empty());
    }

    #[test]
    fn document_body_markdown_is_plain_text() {
        let b = DocumentBody::Markdown("# hi\nbody".into());
        assert_eq!(b.as_plain_text(), "# hi\nbody");
    }

    #[test]
    fn chunk_new_assigns_unique_id() {
        let now = Utc::now();
        let a = Chunk::new("s", "d", 0, "t", vec![], (0, 1), now);
        let b = Chunk::new("s", "d", 0, "t", vec![], (0, 1), now);
        assert_ne!(a.chunk_id, b.chunk_id);
    }
}
