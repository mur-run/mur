//! Qdrant-backed implementation of `VectorStore`.
//!
//! Collection name: "mur_sources". Payload mirrors the LanceDB sources-table
//! columns (source_id, external_id, text, heading_path, char_start, char_end,
//! updated_at_ms). Vector lives in the primary `vector` field.
//!
//! Users run Qdrant via `docker compose -f docker/qdrant-compose.yml up -d`
//! (or a managed instance). mur connects via `storage.qdrant_url` in
//! `~/.mur/config.yaml`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    Condition, CountPointsBuilder, CreateCollectionBuilder, DeletePointsBuilder, Distance, Filter,
    PointStruct, ScrollPointsBuilder, SearchPointsBuilder, UpsertPointsBuilder,
    VectorParamsBuilder, points_selector::PointsSelectorOneOf,
};

use super::{EmbeddedChunk, Hit, SearchFilter, VectorStore};

const COLLECTION: &str = "mur_sources";

pub struct QdrantStore {
    client: Qdrant,
    dimensions: u64,
}

impl QdrantStore {
    pub async fn open(url: &str, dimensions: i32) -> Result<Self> {
        let client = Qdrant::from_url(url).build().context("connect qdrant")?;
        let store = Self {
            client,
            dimensions: dimensions as u64,
        };
        store.ensure_collection().await?;
        Ok(store)
    }

    async fn ensure_collection(&self) -> Result<()> {
        let existing = self.client.list_collections().await?;
        let exists = existing.collections.iter().any(|c| c.name == COLLECTION);
        if exists {
            return Ok(());
        }
        self.client
            .create_collection(
                CreateCollectionBuilder::new(COLLECTION)
                    .vectors_config(VectorParamsBuilder::new(self.dimensions, Distance::Cosine)),
            )
            .await
            .context("create qdrant collection")?;
        Ok(())
    }
}

#[async_trait]
impl VectorStore for QdrantStore {
    async fn upsert(&self, chunks: &[EmbeddedChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let points: Vec<PointStruct> = chunks
            .iter()
            .map(|c| {
                PointStruct::new(
                    c.chunk_id.clone(),
                    c.embedding.clone(),
                    [
                        ("source_id", c.source_id.clone().into()),
                        ("external_id", c.external_id.clone().into()),
                        ("text", c.text.clone().into()),
                        (
                            "heading_path",
                            serde_json::to_string(&c.heading_path)
                                .unwrap_or_default()
                                .into(),
                        ),
                        ("char_start", (c.char_range.0 as i64).into()),
                        ("char_end", (c.char_range.1 as i64).into()),
                        ("updated_at_ms", c.updated_at.timestamp_millis().into()),
                    ],
                )
            })
            .collect();

        self.client
            .upsert_points(UpsertPointsBuilder::new(COLLECTION, points).wait(true))
            .await
            .context("qdrant upsert")?;
        Ok(())
    }

    async fn search(&self, query_vec: &[f32], k: usize, filter: &SearchFilter) -> Result<Vec<Hit>> {
        let mut conditions: Vec<Condition> = Vec::new();
        if let Some(ids) = &filter.source_ids {
            for id in ids {
                conditions.push(Condition::matches("source_id", id.clone()));
            }
        }
        if let Some(since) = filter.since {
            conditions.push(Condition::range(
                "updated_at_ms",
                qdrant_client::qdrant::Range {
                    gte: Some(since.timestamp_millis() as f64),
                    ..Default::default()
                },
            ));
        }

        let mut builder =
            SearchPointsBuilder::new(COLLECTION, query_vec.to_vec(), k as u64).with_payload(true);
        if !conditions.is_empty() {
            builder = builder.filter(Filter::should(conditions));
        }
        let resp = self
            .client
            .search_points(builder)
            .await
            .context("qdrant search")?;

        let mut out: Vec<Hit> = Vec::new();
        for scored in resp.result {
            let pid = scored
                .id
                .clone()
                .and_then(|id| id.point_id_options)
                .map(|opt| match opt {
                    qdrant_client::qdrant::point_id::PointIdOptions::Uuid(s) => s,
                    qdrant_client::qdrant::point_id::PointIdOptions::Num(n) => n.to_string(),
                })
                .unwrap_or_default();
            let payload = &scored.payload;
            let source_id = payload
                .get("source_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned())
                .unwrap_or_default();
            let external_id = payload
                .get("external_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned())
                .unwrap_or_default();
            let text = payload
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned())
                .unwrap_or_default();
            let heading_path: Vec<String> = payload
                .get("heading_path")
                .and_then(|v| v.as_str())
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let updated_at_ms = payload
                .get("updated_at_ms")
                .and_then(|v| v.as_integer())
                .unwrap_or(0);
            let updated_at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(updated_at_ms)
                .unwrap_or_else(chrono::Utc::now);
            out.push(Hit {
                chunk_id: pid,
                source_id,
                external_id,
                score: scored.score,
                text,
                heading_path,
                updated_at,
            });
        }
        Ok(out)
    }

    async fn delete_by_external_ids(&self, source_id: &str, external_ids: &[String]) -> Result<()> {
        if external_ids.is_empty() {
            return Ok(());
        }
        // Build an OR of (source_id match AND external_id match) per external_id.
        // Simpler: delete all chunks where source_id matches AND external_id is in the list.
        // Qdrant doesn't have "IN" natively with must, so we use should for external_ids
        // combined with a must for source_id via nested filter.
        let source_cond = Condition::matches("source_id", source_id.to_string());
        let ext_conditions: Vec<Condition> = external_ids
            .iter()
            .map(|eid| Condition::matches("external_id", eid.clone()))
            .collect();
        // must[source_id] AND should[external_ids...] — use nested approach:
        let filter = Filter {
            must: vec![source_cond],
            should: ext_conditions,
            ..Default::default()
        };
        self.client
            .delete_points(
                DeletePointsBuilder::new(COLLECTION)
                    .points(PointsSelectorOneOf::Filter(filter))
                    .wait(true),
            )
            .await
            .context("qdrant delete_by_external_ids")?;
        Ok(())
    }

    async fn delete_by_source(&self, source_id: &str) -> Result<()> {
        let filter = Filter::must(vec![Condition::matches("source_id", source_id.to_string())]);
        self.client
            .delete_points(
                DeletePointsBuilder::new(COLLECTION)
                    .points(PointsSelectorOneOf::Filter(filter))
                    .wait(true),
            )
            .await
            .context("qdrant delete_by_source")?;
        Ok(())
    }

    async fn list_external_ids(&self, source_id: &str) -> Result<Vec<String>> {
        let filter = Filter::must(vec![Condition::matches("source_id", source_id.to_string())]);
        let mut out: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut offset: Option<qdrant_client::qdrant::PointId> = None;
        loop {
            let mut builder = ScrollPointsBuilder::new(COLLECTION)
                .filter(filter.clone())
                .limit(256)
                .with_payload(true);
            if let Some(o) = offset.clone() {
                builder = builder.offset(o);
            }
            let resp = self.client.scroll(builder).await.context("qdrant scroll")?;
            for p in &resp.result {
                if let Some(ext) = p.payload.get("external_id").and_then(|v| v.as_str()) {
                    out.insert(ext.clone());
                }
            }
            match resp.next_page_offset {
                None => break,
                Some(next) => offset = Some(next),
            }
        }
        Ok(out.into_iter().collect())
    }

    async fn count(&self, source_id: Option<&str>) -> Result<usize> {
        let mut builder = CountPointsBuilder::new(COLLECTION).exact(true);
        if let Some(sid) = source_id {
            builder = builder.filter(Filter::must(vec![Condition::matches(
                "source_id",
                sid.to_string(),
            )]));
        }
        let resp = self.client.count(builder).await?;
        Ok(resp.result.map(|r| r.count as usize).unwrap_or(0))
    }

    async fn rebuild_index(&self) -> Result<()> {
        // Qdrant rebuilds transparently when new points arrive. For a full
        // rebuild, callers drop the collection and re-upsert — handled by
        // `mur reindex` at a higher level.
        Ok(())
    }
}
