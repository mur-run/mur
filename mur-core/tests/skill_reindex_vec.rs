//! Integration tests for skill reindex-vec (M6c.1).
//!
//! Verifies LanceDB operations used by reindex-vec: idempotent upsert,
//! list_external_ids, delete_by_external_ids. Full end-to-end reindex-vec
//! tests require a running embedder and are exercised via the CLI.

use std::path::Path;

use mur_common::config::Config;
use mur_core::skill_index::SKILL_SOURCE_ID;
use mur_core::store::vector::{VectorStore, factory::get_vector_store};
use tempfile::TempDir;

fn test_embedding(dim: usize) -> Vec<f32> {
    (0..dim).map(|i| (i as f32 * 0.01).sin()).collect()
}

async fn setup_store(dir: &Path, dims: i32) -> std::sync::Arc<dyn VectorStore> {
    let mut cfg = Config::default();
    cfg.embedding.dimensions = dims as usize;
    get_vector_store(&cfg, dir).await.unwrap()
}

#[tokio::test]
async fn upsert_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let store = setup_store(tmp.path(), 64).await;
    let dim = 64;

    // We bypass the real embedder — insert chunks directly.
    use mur_core::store::vector::EmbeddedChunk;
    let chunk = EmbeddedChunk {
        chunk_id: "skill:skill-a:1.0.0".into(),
        source_id: SKILL_SOURCE_ID.into(),
        external_id: "skill-a".into(),
        ordinal: 0,
        text: "skill-a\nFirst skill\nAbstract for skill-a".into(),
        heading_path: vec![],
        char_range: (0, 0),
        updated_at: chrono::Utc::now(),
        embedding: test_embedding(dim),
    };
    store.upsert(&[chunk]).await.unwrap();

    // Second upsert with same chunk_id is idempotent — different embedding.
    let chunk2 = EmbeddedChunk {
        chunk_id: "skill:skill-a:1.0.0".into(),
        source_id: SKILL_SOURCE_ID.into(),
        external_id: "skill-a".into(),
        ordinal: 0,
        text: "skill-a\nFirst skill\nAbstract for skill-a".into(),
        heading_path: vec![],
        char_range: (0, 0),
        updated_at: chrono::Utc::now(),
        embedding: test_embedding(dim),
    };
    store.upsert(&[chunk2]).await.unwrap();

    let ids = store.list_external_ids(SKILL_SOURCE_ID).await.unwrap();
    assert_eq!(ids, vec!["skill-a"]);

    let count = store.count(Some(SKILL_SOURCE_ID)).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn list_and_prune_workflow() {
    let tmp = TempDir::new().unwrap();
    let store = setup_store(tmp.path(), 64).await;
    let dim = 64;

    // Insert A, B, C
    for name in &["skill-a", "skill-b", "skill-c"] {
        let chunk = mur_core::store::vector::EmbeddedChunk {
            chunk_id: format!("skill:{name}:1.0.0"),
            source_id: SKILL_SOURCE_ID.into(),
            external_id: (*name).into(),
            ordinal: 0,
            text: format!("{name} text"),
            heading_path: vec![],
            char_range: (0, 0),
            updated_at: chrono::Utc::now(),
            embedding: test_embedding(dim),
        };
        store.upsert(&[chunk]).await.unwrap();
    }

    let all = store.list_external_ids(SKILL_SOURCE_ID).await.unwrap();
    assert_eq!(all.len(), 3);

    // Prune B
    store
        .delete_by_external_ids(SKILL_SOURCE_ID, &["skill-b".into()])
        .await
        .unwrap();

    let remaining = store.list_external_ids(SKILL_SOURCE_ID).await.unwrap();
    let mut sorted = remaining.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["skill-a", "skill-c"]);
}
