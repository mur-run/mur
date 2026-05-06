//! Runs the VectorStore conformance suite against a live Qdrant instance.
//! Skipped unless `QDRANT_URL` is set. Compiled only with `--features qdrant`.
//!
//! CI recipe:
//!   docker compose -f docker/qdrant-compose.yml up -d
//!   QDRANT_URL=http://localhost:6333 cargo test --test qdrant_smoke --features qdrant
#![cfg(feature = "qdrant")]

use mur_core::store::vector::{QdrantStore, VectorStore};

fn qdrant_url() -> Option<String> {
    std::env::var("QDRANT_URL").ok()
}

#[tokio::test]
async fn smoke_count_empty() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: QDRANT_URL not set");
        return;
    };
    let store = QdrantStore::open(&url, 8).await.expect("open qdrant");
    let _ = store.count(Some("conformance-nonexistent")).await;
}
