//! Sync orchestrator.
//!
//! Drives one adapter through list → fetch → chunk → embed → upsert, updates
//! the `SourceInstance` yaml cursor/stats, and detects deletions via a
//! set-diff against the vector store. P1.2 is single-source / sequential /
//! manual — `--watch` and cross-source parallelism arrive in P1.4.

use anyhow::{Context, Result};
use chrono::Utc;
use std::sync::Arc;

use crate::sources::KnowledgeSource;
use crate::sources::instance::{SourceInstance, SourceInstanceStore, SyncError};
use crate::sources::types::SyncCursor;
use crate::store::embedding::{EmbeddingConfig, embed};
use crate::store::vector::{EmbeddedChunk, VectorStore};

/// High-level summary returned by `sync_source`.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub docs_synced: usize,
    pub chunks_emitted: usize,
    pub docs_deleted: usize,
    pub errors: Vec<String>,
}

/// Run one full sync cycle for a single source.
pub async fn sync_source(
    adapter: &dyn KnowledgeSource,
    instance: &mut SourceInstance,
    instance_store: &SourceInstanceStore,
    vector_store: Arc<dyn VectorStore>,
    embedding_cfg: &EmbeddingConfig,
    full: bool,
) -> Result<SyncReport> {
    let source_id = adapter.id().to_string();
    let cursor_in = if full {
        None
    } else {
        instance
            .sync
            .last_cursor
            .clone()
            .map(SyncCursor)
            .filter(|c| !c.is_empty())
    };

    tracing::info!(source_id = %source_id, full = full, "sync: start");

    let (doc_refs, new_cursor) = adapter
        .list_documents(cursor_in)
        .await
        .context("adapter.list_documents")?;

    let mut report = SyncReport::default();

    for doc_ref in &doc_refs {
        match fetch_chunk_embed_upsert(adapter, doc_ref, &*vector_store, embedding_cfg).await {
            Ok(n_chunks) => {
                report.docs_synced += 1;
                report.chunks_emitted += n_chunks;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::warn!(
                    source_id = %source_id,
                    doc = %doc_ref.external_id,
                    error = %msg,
                    "sync: doc error"
                );
                report.errors.push(msg.clone());
                instance.sync.push_error(SyncError {
                    at: Utc::now(),
                    doc: doc_ref.external_id.clone(),
                    msg,
                });
            }
        }
    }

    // Deletion detection runs only in full mode.
    if full {
        let indexed = vector_store
            .list_external_ids(&source_id)
            .await
            .context("list_external_ids")?;
        let current: std::collections::HashSet<String> =
            doc_refs.iter().map(|d| d.external_id.clone()).collect();
        let deleted: Vec<String> = indexed.into_iter().filter(|id| !current.contains(id)).collect();
        if !deleted.is_empty() {
            vector_store
                .delete_by_external_ids(&source_id, &deleted)
                .await
                .context("delete_by_external_ids")?;
            report.docs_deleted = deleted.len();
        }
    }

    instance.sync.last_cursor = if new_cursor.is_empty() {
        None
    } else {
        Some(new_cursor.0)
    };
    instance.sync.last_sync_at = Some(Utc::now());
    instance.sync.last_error = report.errors.last().cloned();
    instance.stats.doc_count = report.docs_synced as u64;
    instance.stats.chunk_count = report.chunks_emitted as u64;

    instance_store
        .save(instance)
        .context("persist SourceInstance yaml")?;

    tracing::info!(
        source_id = %source_id,
        docs = report.docs_synced,
        chunks = report.chunks_emitted,
        deleted = report.docs_deleted,
        errors = report.errors.len(),
        "sync: complete"
    );

    Ok(report)
}

async fn fetch_chunk_embed_upsert(
    adapter: &dyn KnowledgeSource,
    doc_ref: &crate::sources::types::DocRef,
    vector_store: &dyn VectorStore,
    embedding_cfg: &EmbeddingConfig,
) -> Result<usize> {
    let doc = adapter.fetch(doc_ref).await.context("adapter.fetch")?;
    let chunks = adapter.chunk(&doc).context("adapter.chunk")?;
    if chunks.is_empty() {
        return Ok(0);
    }
    // Delete-by-external_id before upserting (handles the case where the same
    // document's chunk set changed — old chunk_ids no longer valid).
    vector_store
        .delete_by_external_ids(&doc.source_id, &[doc.external_id.clone()])
        .await
        .context("delete old chunks for doc")?;
    let mut embedded: Vec<EmbeddedChunk> = Vec::with_capacity(chunks.len());
    for c in chunks {
        let vec = embed(&c.text, embedding_cfg)
            .await
            .with_context(|| format!("embed chunk of doc {}", doc.external_id))?;
        embedded.push(EmbeddedChunk {
            chunk_id: c.chunk_id,
            source_id: c.source_id,
            external_id: c.external_id,
            ordinal: c.ordinal,
            text: c.text,
            heading_path: c.heading_path,
            char_range: c.char_range,
            updated_at: c.updated_at,
            embedding: vec,
        });
    }
    let n = embedded.len();
    vector_store
        .upsert(&embedded)
        .await
        .context("vector_store.upsert")?;
    Ok(n)
}
