//! Regression tests for #1614: a sync that cannot write must not delete.

use super::*;
use crate::sources::instance::{SourceStats, SyncState};
use crate::sources::types::{Chunk, DocRef, Document, DocumentBody};
use crate::sources::{KnowledgeSource, SourceKind};
use crate::store::embedding::EmbeddingProvider;
use crate::store::vector::LanceDbStore;
use std::collections::BTreeMap;
use tempfile::TempDir;

const DIM: i32 = 16;
// No ':' — the id becomes a yaml filename and NTFS rejects colons
// (see `SourceInstanceStore::path_for`).
const SOURCE_ID: &str = "fake-vault";
const DOC: &str = "alpha.md";

/// One-document adapter; never touches the network or the filesystem.
struct FakeSource;

#[async_trait::async_trait]
impl KnowledgeSource for FakeSource {
    fn id(&self) -> &str {
        SOURCE_ID
    }
    fn kind(&self) -> SourceKind {
        SourceKind::PullIndex
    }
    fn weight(&self) -> f32 {
        1.0
    }
    async fn list_documents(
        &self,
        _cursor: Option<SyncCursor>,
    ) -> Result<(Vec<DocRef>, SyncCursor)> {
        let doc = DocRef {
            external_id: DOC.into(),
            title: None,
            updated_at: Utc::now(),
        };
        Ok((vec![doc], SyncCursor::default()))
    }
    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document> {
        Ok(Document {
            source_id: SOURCE_ID.into(),
            external_id: doc_ref.external_id.clone(),
            title: "alpha".into(),
            body: DocumentBody::Markdown("new body".into()),
            url: None,
            updated_at: Utc::now(),
            tags: vec![],
            metadata: serde_json::Value::Null,
        })
    }
    fn chunk(&self, doc: &Document) -> Result<Vec<Chunk>> {
        Ok(vec![Chunk::new(
            SOURCE_ID,
            doc.external_id.clone(),
            0,
            "new body",
            vec![],
            (0, 8),
            Utc::now(),
        )])
    }
}

fn instance() -> SourceInstance {
    SourceInstance {
        id: SOURCE_ID.into(),
        type_name: "obsidian".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope: BTreeMap::new(),
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry: None,
    }
}

/// Embedding config whose provider can never answer: port 1 refuses at once.
fn dead_embedder(dims: i32) -> EmbeddingConfig {
    EmbeddingConfig {
        provider: EmbeddingProvider::Ollama {
            base_url: "http://127.0.0.1:1".into(),
        },
        model: "none".into(),
        dimensions: dims as usize,
        batch_size: 1,
    }
}

/// Seed the sources table at `dims` with one row for `DOC`.
async fn seed(index: &std::path::Path, dims: i32) {
    let store = LanceDbStore::open(index, dims).await.unwrap();
    let row = EmbeddedChunk {
        chunk_id: "old-chunk".into(),
        source_id: SOURCE_ID.into(),
        external_id: DOC.into(),
        ordinal: 0,
        text: "old body".into(),
        heading_path: vec![],
        char_range: (0, 8),
        updated_at: Utc::now(),
        embedding: vec![0.5; dims as usize],
    };
    store.upsert(&[row]).await.unwrap();
}

async fn rows_for_source(index: &std::path::Path, dims: i32) -> usize {
    let store = LanceDbStore::open(index, dims).await.unwrap();
    store.count(Some(SOURCE_ID)).await.unwrap()
}

#[tokio::test]
async fn width_mismatch_aborts_before_any_delete() {
    let tmp = TempDir::new().unwrap();
    let index = tmp.path().join("index");
    // An old 2x-wide table that already holds this source's document...
    seed(&index, DIM * 2).await;

    // ...synced after the configured dimension changed.
    let store: Arc<dyn VectorStore> = Arc::new(LanceDbStore::open(&index, DIM).await.unwrap());
    let tantivy = crate::sources::tantivy::TantivyIndex::open_or_create(tmp.path()).unwrap();
    let instances = SourceInstanceStore::new(tmp.path().join("sources"));
    let mut inst = instance();

    for full in [false, true] {
        let err = sync_source(
            &FakeSource,
            &mut inst,
            &instances,
            store.clone(),
            &tantivy,
            &dead_embedder(DIM),
            full,
        )
        .await
        .expect_err("a sync that cannot write must fail, not report per-doc errors");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("reindex-vec"),
            "error must name the fix: {msg}"
        );
        assert_eq!(
            rows_for_source(&index, DIM * 2).await,
            1,
            "full={full}: existing chunks must survive the rejected sync"
        );
    }
}

#[tokio::test]
async fn embedding_failure_keeps_existing_chunks() {
    let tmp = TempDir::new().unwrap();
    let index = tmp.path().join("index");
    seed(&index, DIM).await;

    let store: Arc<dyn VectorStore> = Arc::new(LanceDbStore::open(&index, DIM).await.unwrap());
    let tantivy = crate::sources::tantivy::TantivyIndex::open_or_create(tmp.path()).unwrap();
    let instances = SourceInstanceStore::new(tmp.path().join("sources"));
    let mut inst = instance();

    let report = sync_source(
        &FakeSource,
        &mut inst,
        &instances,
        store,
        &tantivy,
        &dead_embedder(DIM),
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        report.errors.len(),
        1,
        "the doc must be reported: {report:?}"
    );
    assert_eq!(report.docs_synced, 0);
    assert_eq!(
        rows_for_source(&index, DIM).await,
        1,
        "an embedding outage must not delete the doc's old chunks"
    );
}
