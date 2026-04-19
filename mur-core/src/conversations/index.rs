//! LanceDB index for the conversations archive. Table "conversations".
//! See spec §4.4.
//!
//! Observability (BP6): `upsert` and `search` are each wrapped in a
//! `tracing::info_span!` so `RUST_LOG=mur_core::conversations=info` yields
//! per-call timing breakdowns.

use anyhow::{Context, Result};
use arrow_array::{
    FixedSizeListArray, Float32Array, Int8Array, Int64Array, RecordBatch, RecordBatchIterator,
    StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use mur_common::{Message, Source};
use std::sync::Arc;
use tracing::info_span;

use super::paths::index_path;

const TABLE: &str = "conversations";

pub struct ConversationIndex {
    db: lancedb::Connection,
    dims: i32,
}

#[derive(Debug)]
pub struct SearchHit {
    pub id: String,
    pub ts: i64,
    pub source: Source,
    pub conv_id: String,
    pub content: String,
    pub distance: f32,
}

impl ConversationIndex {
    pub async fn open(dims: i32, root_override: Option<&str>) -> Result<Self> {
        let path = index_path(root_override);
        std::fs::create_dir_all(path.parent().unwrap())?;
        let db = lancedb::connect(path.to_str().unwrap())
            .execute()
            .await
            .context("opening LanceDB for conversations")?;
        Ok(Self { db, dims })
    }

    fn schema(&self) -> Schema {
        Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("ts", DataType::Int64, false),
            Field::new("source", DataType::Utf8, false),
            Field::new("conv_id", DataType::Utf8, false),
            Field::new("role", DataType::Utf8, false),
            Field::new("layer", DataType::Int8, false),
            Field::new("content", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    self.dims,
                ),
                false,
            ),
        ])
    }

    pub async fn upsert(&mut self, entries: &[(Message, Vec<f32>)]) -> Result<()> {
        let _span = info_span!("conversations.index.upsert", count = entries.len()).entered();
        if entries.is_empty() {
            return Ok(());
        }
        let schema = Arc::new(self.schema());
        let tables = self.db.table_names().execute().await?;

        let ids: Vec<String> = entries
            .iter()
            .enumerate()
            .map(|(i, (m, _))| format!("{}_{}_{}", m.src.file_prefix(), m.conv, i))
            .collect();
        let tss: Vec<i64> = entries.iter().map(|(m, _)| m.ts.timestamp()).collect();
        let srcs: Vec<&str> = entries.iter().map(|(m, _)| m.src.file_prefix()).collect();
        let convs: Vec<&str> = entries.iter().map(|(m, _)| m.conv.as_str()).collect();
        let roles: Vec<&'static str> = entries
            .iter()
            .map(|(m, _)| match m.role {
                mur_common::Role::User => "user",
                mur_common::Role::Assistant => "assistant",
                mur_common::Role::System => "system",
                mur_common::Role::Tool => "tool",
            })
            .collect();
        let layers: Vec<i8> = entries.iter().map(|_| 0_i8).collect();
        let contents: Vec<String> = entries
            .iter()
            .map(|(m, _)| m.content.as_text().to_owned())
            .collect();
        let content_refs: Vec<&str> = contents.iter().map(|s| s.as_str()).collect();

        let flat: Vec<f32> = entries
            .iter()
            .flat_map(|(_, v)| v.iter().copied())
            .collect();
        let vec_arr = FixedSizeListArray::try_new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            self.dims,
            Arc::new(Float32Array::from(flat)),
            None,
        )?;

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(
                    ids.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                )),
                Arc::new(Int64Array::from(tss)),
                Arc::new(StringArray::from(srcs)),
                Arc::new(StringArray::from(convs)),
                Arc::new(StringArray::from(roles)),
                Arc::new(Int8Array::from(layers)),
                Arc::new(StringArray::from(content_refs)),
                Arc::new(vec_arr),
            ],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema.clone());
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(batches);

        if tables.contains(&TABLE.to_string()) {
            self.db
                .open_table(TABLE)
                .execute()
                .await?
                .add(reader)
                .execute()
                .await?;
        } else {
            self.db.create_table(TABLE, reader).execute().await?;
        }
        Ok(())
    }

    pub async fn search(
        &self,
        query_vec: &[f32],
        limit: usize,
        source_filter: Option<Source>,
    ) -> Result<Vec<SearchHit>> {
        let _span = info_span!(
            "conversations.index.search",
            k = limit,
            source = ?source_filter
        )
        .entered();
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&TABLE.to_string()) {
            return Ok(Vec::new());
        }
        let table = self.db.open_table(TABLE).execute().await?;
        let mut q = table.query().nearest_to(query_vec)?.limit(limit);
        if let Some(s) = source_filter {
            q = q.only_if(format!("source = '{}'", s.file_prefix()));
        }
        let stream = q.execute().await?;
        let batches: Vec<RecordBatch> = stream.try_collect().await?;
        let mut out = Vec::new();
        for b in batches {
            let ids = b
                .column_by_name("id")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let tss = b
                .column_by_name("ts")
                .unwrap()
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap();
            let srcs = b
                .column_by_name("source")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let convs = b
                .column_by_name("conv_id")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let contents = b
                .column_by_name("content")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let dists = b
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
            for i in 0..b.num_rows() {
                let source = match srcs.value(i) {
                    "cc" => Source::ClaudeCode,
                    "cursor" => Source::Cursor,
                    "gemini" => Source::Gemini,
                    "aider" => Source::Aider,
                    "slack" => Source::Slack,
                    "telegram" => Source::Telegram,
                    "discord" => Source::Discord,
                    "commander" => Source::CommanderEngine,
                    other => anyhow::bail!("unknown source tag {other}"),
                };
                out.push(SearchHit {
                    id: ids.value(i).to_string(),
                    ts: tss.value(i),
                    source,
                    conv_id: convs.value(i).to_string(),
                    content: contents.value(i).to_string(),
                    distance: dists.map(|d| d.value(i)).unwrap_or(0.0),
                });
            }
        }
        Ok(out)
    }

    /// Build/refresh a RaBitQ index on the vector column. Call periodically
    /// (e.g., nightly) once the table exceeds ~10k rows. For Phase 1 this is
    /// a no-op stub; the flat search path is sufficient.
    pub async fn rebuild_rabitq(&self) -> Result<()> {
        let _span = info_span!("conversations.index.rebuild_rabitq").entered();
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&TABLE.to_string()) {
            return Ok(());
        }
        let _table = self.db.open_table(TABLE).execute().await?;
        // LanceDB 0.26: `create_index` with IvfPq / Hnsw. RaBitQ is available
        // via IndexType::RaBitQ when the feature is present; fall back to IVF_PQ.
        // TODO(phase-2): pin the exact call once LanceDB API stabilizes.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Content, Message, Role, Source};

    fn msg(n: &str, text: &str) -> Message {
        Message {
            v: 1,
            ts: chrono::Utc::now(),
            src: Source::ClaudeCode,
            conv: n.into(),
            role: Role::User,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[tokio::test]
    async fn open_and_upsert_and_search() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        let entries = vec![
            (msg("a", "cargo build failed"), vec![1.0; 16]),
            (msg("b", "yaml parsing worked"), vec![0.0; 16]),
        ];
        idx.upsert(&entries).await.unwrap();
        let hits = idx.search(&vec![1.0; 16], 2, None).await.unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].conv_id, "a");
    }

    #[tokio::test]
    async fn filter_by_source() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        let mut m = msg("x", "shared");
        m.src = Source::Slack;
        idx.upsert(&[(m, vec![1.0; 16])]).await.unwrap();
        idx.upsert(&[(msg("y", "shared"), vec![1.0; 16])])
            .await
            .unwrap();
        let hits = idx
            .search(&vec![1.0; 16], 10, Some(Source::Slack))
            .await
            .unwrap();
        assert!(hits.iter().all(|h| h.source == Source::Slack));
    }
}
