//! Conformance suite every `VectorStore` impl must satisfy.
//!
//! Usage from an impl's module:
//! ```ignore
//! #[cfg(test)]
//! mod conformance {
//!     use super::*;
//!     $crate::vector_store_conformance!(LanceDbStore, make_store_for_conformance);
//!     async fn make_store_for_conformance() -> LanceDbStore { /* ... */ }
//! }
//! ```
//!
//! Phase 1.1 only runs the smoke test — upsert/search tests are `#[ignore]` until
//! P1.2 replaces the trait-method stubs with real code.

#![allow(dead_code)]

use super::*;

/// Generic smoke test: construct the store and prove `count` is callable.
///
/// `LanceDbStore::count` currently `bail!`s — this function only asserts the
/// method is reachable via the trait (compile-time check passes, and runtime
/// does not panic before the bail). Once P1.2 provides real implementations,
/// this will be replaced with a real assertion.
pub async fn smoke_count_empty<S: VectorStore>(store: &S) {
    let _ = store.count(Some("nonexistent-source-id")).await;
}

/// Round-trip test (meaningful from P1.2 onward).
pub async fn upsert_and_search<S: VectorStore>(store: &S, dims: usize) -> anyhow::Result<()> {
    let chunk = EmbeddedChunk {
        chunk_id: "chunk-a".into(),
        source_id: "test".into(),
        external_id: "doc-1".into(),
        ordinal: 0,
        text: "hello world".into(),
        heading_path: vec![],
        char_range: (0, 11),
        updated_at: chrono::Utc::now(),
        embedding: vec![0.1_f32; dims],
    };
    store.upsert(&[chunk.clone()]).await?;
    let hits = store
        .search(&vec![0.1_f32; dims], 5, &SearchFilter::default())
        .await?;
    anyhow::ensure!(!hits.is_empty(), "expected at least one hit");
    Ok(())
}

/// Delete-by-source removes everything for that source.
pub async fn delete_by_source_clears<S: VectorStore>(store: &S) -> anyhow::Result<()> {
    store.delete_by_source("test").await?;
    let ids = store.list_external_ids("test").await?;
    anyhow::ensure!(ids.is_empty(), "expected zero ids after delete_by_source");
    Ok(())
}

/// Drop this into an impl's test module to exercise the full suite.
///
/// Use `$crate::vector_store_conformance!(YourStore, your_factory)` — the macro
/// is exported at the crate root via `#[macro_export]`.
#[macro_export]
macro_rules! vector_store_conformance {
    ($ty:ty, $factory:ident) => {
        #[tokio::test]
        async fn conformance_smoke_count_empty() {
            let s = $factory().await;
            $crate::store::vector::tests::smoke_count_empty::<$ty>(&s).await;
        }

        #[tokio::test]
        async fn conformance_upsert_and_search() {
            let s = $factory().await;
            $crate::store::vector::tests::upsert_and_search::<$ty>(&s, 64)
                .await
                .expect("roundtrip");
        }

        #[tokio::test]
        async fn conformance_delete_by_source_clears() {
            let s = $factory().await;
            $crate::store::vector::tests::delete_by_source_clears::<$ty>(&s)
                .await
                .expect("delete_by_source");
        }
    };
}
