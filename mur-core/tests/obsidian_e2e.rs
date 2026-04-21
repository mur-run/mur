//! End-to-end: ObsidianAdapter + chunker + LanceDbStore.
//!
//! Does not exercise the embedding step (uses deterministic fake vectors).
//! Real embeddings are covered by manual smoke tests and adapter unit tests.

use mur_core::sources::KnowledgeSource;
use mur_core::sources::adapters::obsidian::ObsidianAdapter;
use mur_core::sources::instance::{SourceInstance, SourceStats, SyncState};
use mur_core::sources::kind::SourceKind;
use mur_core::store::vector::{EmbeddedChunk, LanceDbStore, SearchFilter, VectorStore};
use std::collections::BTreeMap;
use std::fs;
use tempfile::TempDir;

fn make_instance(id: &str, vault: &std::path::Path) -> SourceInstance {
    let mut scope = BTreeMap::new();
    scope.insert(
        "vault".into(),
        serde_yaml::Value::String(vault.to_string_lossy().to_string()),
    );
    SourceInstance {
        id: id.into(),
        type_name: "obsidian".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry: None,
    }
}

const DIM: i32 = 8;

fn zeros() -> Vec<f32> {
    vec![0.0_f32; DIM as usize]
}
fn ones() -> Vec<f32> {
    vec![1.0_f32; DIM as usize]
}

#[tokio::test]
async fn obsidian_end_to_end_sync_then_search() {
    let vault = TempDir::new().unwrap();
    fs::create_dir_all(vault.path().join(".obsidian")).unwrap();
    fs::write(
        vault.path().join("design.md"),
        "---\ntags: [design]\n---\n\n# Auth design\n\nJWT 15min access + 7d refresh.",
    )
    .unwrap();
    fs::write(
        vault.path().join("scratch.md"),
        "# scratch\n\nnothing interesting",
    )
    .unwrap();

    let inst = make_instance("obsidian:e2e", vault.path());
    let adapter = ObsidianAdapter::from_instance(&inst).unwrap();

    let (refs, _cursor) = adapter.list_documents(None).await.unwrap();
    assert_eq!(refs.len(), 2);

    let index = TempDir::new().unwrap();
    let store = LanceDbStore::open(index.path(), DIM).await.unwrap();
    store.ensure_sources_table().await.unwrap();

    let mut all_chunks: Vec<EmbeddedChunk> = Vec::new();
    for r in &refs {
        let doc = adapter.fetch(r).await.unwrap();
        for c in adapter.chunk(&doc).unwrap() {
            let embed = if c.external_id.contains("design") {
                ones()
            } else {
                zeros()
            };
            all_chunks.push(EmbeddedChunk {
                chunk_id: c.chunk_id,
                source_id: c.source_id,
                external_id: c.external_id,
                ordinal: c.ordinal,
                text: c.text,
                heading_path: c.heading_path,
                char_range: c.char_range,
                updated_at: c.updated_at,
                embedding: embed,
            });
        }
    }
    store.upsert(&all_chunks).await.unwrap();

    let hits = <LanceDbStore as VectorStore>::search(&store, &ones(), 5, &SearchFilter::default())
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].external_id, "design.md");

    let ids = store.list_external_ids("obsidian:e2e").await.unwrap();
    assert!(ids.contains(&"design.md".to_string()));
    assert!(ids.contains(&"scratch.md".to_string()));

    let c = store.count(Some("obsidian:e2e")).await.unwrap();
    assert!(c >= 2);

    fs::remove_file(vault.path().join("scratch.md")).unwrap();
    let (refs_after, _) = adapter.list_documents(None).await.unwrap();
    let current: std::collections::HashSet<String> =
        refs_after.iter().map(|r| r.external_id.clone()).collect();
    let indexed = store.list_external_ids("obsidian:e2e").await.unwrap();
    let deleted: Vec<String> = indexed
        .into_iter()
        .filter(|id| !current.contains(id))
        .collect();
    assert_eq!(deleted, vec!["scratch.md".to_string()]);
    store
        .delete_by_external_ids("obsidian:e2e", &deleted)
        .await
        .unwrap();
    let after = store.list_external_ids("obsidian:e2e").await.unwrap();
    assert!(!after.contains(&"scratch.md".to_string()));
    assert!(after.contains(&"design.md".to_string()));

    store.delete_by_source("obsidian:e2e").await.unwrap();
    let empty = store.list_external_ids("obsidian:e2e").await.unwrap();
    assert!(empty.is_empty());

    std::mem::forget(index);
    std::mem::forget(vault);
}
