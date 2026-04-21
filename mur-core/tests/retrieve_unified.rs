//! Deterministic corpus + ranking assertions across vector + BM25.
//!
//! Bypasses the embed step (uses fake fixed-dimension vectors) so the test
//! runs without an Ollama / OpenAI provider. Asserts the same chunk wins on
//! both axes when content is aligned.

#[cfg(feature = "sources")]
use chrono::Utc;
#[cfg(feature = "sources")]
use mur_core::sources::tantivy::TantivyIndex;
#[cfg(feature = "sources")]
use mur_core::store::vector::{EmbeddedChunk, LanceDbStore, SearchFilter, VectorStore};
#[cfg(feature = "sources")]
use std::sync::Arc;
#[cfg(feature = "sources")]
use tempfile::TempDir;

#[cfg(feature = "sources")]
const DIM: i32 = 8;

#[cfg(feature = "sources")]
fn chunk(cid: &str, sid: &str, ext: &str, text: &str, embed: Vec<f32>) -> EmbeddedChunk {
    EmbeddedChunk {
        chunk_id: cid.into(),
        source_id: sid.into(),
        external_id: ext.into(),
        ordinal: 0,
        text: text.into(),
        heading_path: vec![],
        char_range: (0, text.len()),
        updated_at: Utc::now(),
        embedding: embed,
    }
}

#[cfg(feature = "sources")]
#[tokio::test]
async fn vec_and_bm25_agree_on_top_chunk() {
    let index_dir = TempDir::new().unwrap();
    let lancedb = LanceDbStore::open(index_dir.path(), DIM).await.unwrap();
    lancedb.ensure_sources_table().await.unwrap();
    let store: Arc<dyn VectorStore> = Arc::new(lancedb);

    let tantivy_dir = TempDir::new().unwrap();
    let tantivy = TantivyIndex::open_or_create(tantivy_dir.path()).unwrap();

    let ones = vec![1.0_f32; DIM as usize];
    let zeros = vec![0.0_f32; DIM as usize];

    store
        .upsert(&[
            chunk(
                "c1",
                "o:a",
                "match.md",
                "rust async tokio programming",
                ones.clone(),
            ),
            chunk(
                "c2",
                "o:b",
                "other.md",
                "JVM garbage collection",
                zeros.clone(),
            ),
        ])
        .await
        .unwrap();
    tantivy
        .upsert(&[
            (
                "c1".into(),
                "o:a".into(),
                "match.md".into(),
                "rust async tokio programming".into(),
            ),
            (
                "c2".into(),
                "o:b".into(),
                "other.md".into(),
                "JVM garbage collection".into(),
            ),
        ])
        .unwrap();

    let vec_hits = store
        .search(&ones, 5, &SearchFilter::default())
        .await
        .unwrap();
    let bm = tantivy.search("rust tokio", 5, None).unwrap();

    assert!(!vec_hits.is_empty());
    assert_eq!(vec_hits[0].chunk_id, "c1");
    assert!(!bm.is_empty());
    assert_eq!(bm[0].chunk_id, "c1");

    // Avoid TempDir-drop race against LanceDB's open handle.
    std::mem::forget(index_dir);
    std::mem::forget(tantivy_dir);
}

#[cfg(not(feature = "sources"))]
#[test]
fn smoke_skipped_without_sources_feature() {
    eprintln!("skipped: sources feature is off");
}
