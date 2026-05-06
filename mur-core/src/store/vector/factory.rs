//! Vector store factory.
//!
//! Returns an `Arc<dyn VectorStore>` selected by `Config::storage::vector_backend`.
//! Default is `"lancedb"`. `"qdrant"` requires building with `--features qdrant`.

use anyhow::{Context, Result, bail};
use mur_common::config::Config;
use std::path::Path;
use std::sync::Arc;

use super::VectorStore;
use super::lancedb::LanceDbStore;

#[allow(dead_code)] // called by sources module in P1.2+; factory tests exercise it directly
pub async fn get_vector_store(cfg: &Config, index_dir: &Path) -> Result<Arc<dyn VectorStore>> {
    match cfg.storage.vector_backend.as_str() {
        "lancedb" => {
            let store = LanceDbStore::open(index_dir, cfg.embedding.dimensions as i32)
                .await
                .context("opening LanceDB vector store")?;
            Ok(Arc::new(store))
        }
        #[cfg(feature = "qdrant")]
        "qdrant" => {
            let url = cfg
                .storage
                .qdrant_url
                .clone()
                .context("storage.qdrant_url required when vector_backend = qdrant")?;
            let store = super::qdrant::QdrantStore::open(&url, cfg.embedding.dimensions as i32)
                .await
                .context("opening Qdrant vector store")?;
            Ok(Arc::new(store))
        }
        #[cfg(not(feature = "qdrant"))]
        "qdrant" => bail!(
            "vector_backend = \"qdrant\" requires building mur with `--features qdrant`. \
             Default builds use lancedb to keep CI fast (qdrant-client adds ~3-5 min compile)."
        ),
        other => bail!("unknown storage.vector_backend: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn dims_128_cfg() -> Config {
        let mut c = Config::default();
        c.embedding.dimensions = 128;
        c
    }

    #[tokio::test]
    async fn factory_returns_lancedb_by_default() {
        let tmp = TempDir::new().unwrap();
        let cfg = dims_128_cfg();
        let store = get_vector_store(&cfg, tmp.path()).await.unwrap();
        // count() is stubbed (bail!s); we only assert the method is callable.
        let _ = store.count(None).await;
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn factory_qdrant_requires_url() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = dims_128_cfg();
        cfg.storage.vector_backend = "qdrant".into();
        // No qdrant_url set → factory returns an error about missing URL.
        let err = get_vector_store(&cfg, tmp.path()).await.err().unwrap();
        assert!(
            err.to_string().contains("qdrant_url"),
            "expected qdrant_url error, got: {err}"
        );
    }

    #[cfg(not(feature = "qdrant"))]
    #[tokio::test]
    async fn factory_qdrant_without_feature_emits_helpful_error() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = dims_128_cfg();
        cfg.storage.vector_backend = "qdrant".into();
        let err = get_vector_store(&cfg, tmp.path()).await.err().unwrap();
        assert!(
            err.to_string().contains("--features qdrant"),
            "expected feature-gate hint, got: {err}"
        );
    }

    #[tokio::test]
    async fn factory_rejects_unknown_backend() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = dims_128_cfg();
        cfg.storage.vector_backend = "bogus".into();
        let err = get_vector_store(&cfg, tmp.path()).await.err().unwrap();
        assert!(err.to_string().contains("unknown storage.vector_backend"));
    }
}
