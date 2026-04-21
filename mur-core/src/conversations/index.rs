//! LanceDB index for the conversations archive. Table "conversations".
//! See spec §4.4.
//!
//! Observability (BP6): `upsert` and `search` are each wrapped in a
//! `tracing::info_span!` so `RUST_LOG=mur_core::conversations=info` yields
//! per-call timing breakdowns.
#![allow(dead_code)] // Phase 1: stubs wired in by later tasks (migrate, retrieve).

use anyhow::{Context, Result};
use arrow_array::{
    FixedSizeListArray, Float32Array, Int8Array, Int64Array, RecordBatch, RecordBatchIterator,
    StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use mur_common::{Message, Source};
use std::sync::Arc;
use tracing::info_span;

use super::paths::index_path;

const TABLE: &str = "conversations";

fn parse_source_or_placeholder(s: &str) -> Source {
    match s {
        "cc" => Source::ClaudeCode,
        "cursor" => Source::Cursor,
        "gemini" => Source::Gemini,
        "aider" => Source::Aider,
        "slack" => Source::Slack,
        "telegram" => Source::Telegram,
        "discord" => Source::Discord,
        "commander" => Source::CommanderEngine,
        _ => Source::ClaudeCode,
    }
}

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
    pub layer: i8,
    pub vector: Option<Vec<f32>>,
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

    /// Upsert with an explicit `layer` value per entry. Phase 2A uses this for
    /// summary rows (layer=1); existing raw writes continue via `upsert()`
    /// which keeps layer=0 for backward compatibility.
    pub async fn upsert_with_layer(&mut self, entries: &[(Message, Vec<f32>, i8)]) -> Result<()> {
        self.upsert_internal(entries).await
    }

    pub async fn upsert(&mut self, batch: &[(Message, Vec<f32>)]) -> Result<()> {
        let with_layer: Vec<(Message, Vec<f32>, i8)> = batch
            .iter()
            .map(|(m, v)| (m.clone(), v.clone(), 0))
            .collect();
        self.upsert_internal(&with_layer).await
    }

    async fn upsert_internal(&mut self, entries: &[(Message, Vec<f32>, i8)]) -> Result<()> {
        let _span = info_span!("conversations.index.upsert", count = entries.len()).entered();
        if entries.is_empty() {
            return Ok(());
        }
        let schema = Arc::new(self.schema());
        let tables = self.db.table_names().execute().await?;

        let ids: Vec<String> = entries
            .iter()
            .enumerate()
            .map(|(i, (m, _, layer))| {
                // Meta can override the batch-index suffix for layer-aware
                // semantic ids (e.g. layer=2 span rows use line_hint).
                let suffix: String = m
                    .meta
                    .get("id_suffix")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| i.to_string());
                if *layer == 0 {
                    format!("{}_{}_{}", m.src.file_prefix(), m.conv, suffix)
                } else {
                    format!("{}_{}_L{}_{}", m.src.file_prefix(), m.conv, layer, suffix)
                }
            })
            .collect();
        let tss: Vec<i64> = entries.iter().map(|(m, _, _)| m.ts.timestamp()).collect();
        let srcs: Vec<&str> = entries
            .iter()
            .map(|(m, _, _)| m.src.file_prefix())
            .collect();
        let convs: Vec<&str> = entries.iter().map(|(m, _, _)| m.conv.as_str()).collect();
        let roles: Vec<&'static str> = entries
            .iter()
            .map(|(m, _, _)| match m.role {
                mur_common::Role::User => "user",
                mur_common::Role::Assistant => "assistant",
                mur_common::Role::System => "system",
                mur_common::Role::Tool => "tool",
            })
            .collect();
        let layers: Vec<i8> = entries.iter().map(|(_, _, l)| *l).collect();
        let contents: Vec<String> = entries
            .iter()
            .map(|(m, _, _)| m.content.as_text().to_owned())
            .collect();
        let content_refs: Vec<&str> = contents.iter().map(|s| s.as_str()).collect();

        let flat: Vec<f32> = entries
            .iter()
            .flat_map(|(_, v, _)| v.iter().copied())
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
        layer: Option<i8>,
    ) -> Result<Vec<SearchHit>> {
        let _span = info_span!(
            "conversations.index.search",
            k = limit,
            source = ?source_filter,
            layer = ?layer
        )
        .entered();
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&TABLE.to_string()) {
            return Ok(Vec::new());
        }
        let table = self.db.open_table(TABLE).execute().await?;
        let mut q = table.query().nearest_to(query_vec)?.limit(limit);
        q = q.select(Select::Columns(vec![
            "id".into(),
            "ts".into(),
            "source".into(),
            "conv_id".into(),
            "role".into(),
            "layer".into(),
            "content".into(),
            "vector".into(),
        ]));

        let predicates: Vec<String> = std::iter::empty::<String>()
            .chain(source_filter.map(|s| format!("source = '{}'", s.file_prefix())))
            .chain(layer.map(|l| format!("layer = {l}")))
            .collect();
        if !predicates.is_empty() {
            q = q.only_if(predicates.join(" AND "));
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
            let layers = b
                .column_by_name("layer")
                .and_then(|c| c.as_any().downcast_ref::<Int8Array>());
            let vectors = b
                .column_by_name("vector")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
            for i in 0..b.num_rows() {
                let source = parse_source_or_placeholder(srcs.value(i));
                let layer = layers.map(|a| a.value(i)).unwrap_or(0);
                let vector = vectors.and_then(|arr| {
                    let fsl = arr.value(i);
                    let floats = fsl.as_any().downcast_ref::<Float32Array>()?;
                    Some(
                        (0..floats.len())
                            .map(|j| floats.value(j))
                            .collect::<Vec<f32>>(),
                    )
                });
                out.push(SearchHit {
                    id: ids.value(i).to_string(),
                    ts: tss.value(i),
                    source,
                    conv_id: convs.value(i).to_string(),
                    content: contents.value(i).to_string(),
                    distance: dists.map(|d| d.value(i)).unwrap_or(0.0),
                    layer,
                    vector,
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

    /// Count rows at a specific layer. Used by doctor to report coverage.
    pub async fn count_rows_at_layer(&self, layer: i8) -> Result<u64> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&TABLE.to_string()) {
            return Ok(0);
        }
        let table = self.db.open_table(TABLE).execute().await?;
        let n = table.count_rows(Some(format!("layer = {layer}"))).await?;
        Ok(n as u64)
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
        let hits = idx.search(&[1.0; 16], 2, None, None).await.unwrap();
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
            .search(&[1.0; 16], 10, Some(Source::Slack), None)
            .await
            .unwrap();
        assert!(hits.iter().all(|h| h.source == Source::Slack));
    }

    #[tokio::test]
    async fn search_filters_by_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();

        let a = msg("a", "layer zero item"); // raw → layer 0
        let b = msg("b", "layer one item"); // summary → layer 1
        idx.upsert_with_layer(&[(a, vec![1.0; 16], 0)])
            .await
            .unwrap();
        idx.upsert_with_layer(&[(b, vec![1.0; 16], 1)])
            .await
            .unwrap();

        let hits_all = idx.search(&[1.0; 16], 10, None, None).await.unwrap();
        assert_eq!(hits_all.len(), 2);

        let hits_l1 = idx.search(&[1.0; 16], 10, None, Some(1)).await.unwrap();
        assert_eq!(hits_l1.len(), 1);
        assert_eq!(hits_l1[0].conv_id, "b");

        let hits_l0 = idx.search(&[1.0; 16], 10, None, Some(0)).await.unwrap();
        assert_eq!(hits_l0.len(), 1);
        assert_eq!(hits_l0[0].conv_id, "a");
    }

    #[tokio::test]
    async fn search_hit_carries_layer_and_vector() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        let m = msg("a", "hello world");
        idx.upsert_with_layer(&[(m, vec![1.0; 16], 2)])
            .await
            .unwrap();
        let hits = idx.search(&[1.0; 16], 1, None, Some(2)).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].layer, 2);
        let v = hits[0].vector.as_ref().expect("vector should be populated");
        assert_eq!(v.len(), 16);
        assert!(v.iter().any(|x| *x > 0.0));
    }

    #[tokio::test]
    async fn upsert_ids_are_layer_aware() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        let m0 = msg("xy", "raw message");
        let m2 = msg("xy", "span text");
        idx.upsert_with_layer(&[(m0, vec![0.5; 16], 0)])
            .await
            .unwrap();
        idx.upsert_with_layer(&[(m2, vec![0.6; 16], 2)])
            .await
            .unwrap();
        let hits_all = idx.search(&[0.55; 16], 10, None, None).await.unwrap();
        assert_eq!(hits_all.len(), 2, "both rows should coexist");
        let ids: Vec<_> = hits_all.iter().map(|h| h.id.as_str()).collect();
        assert!(ids.iter().any(|id| id.contains("_L2_")));
        assert!(ids.iter().any(|id| !id.contains("_L"))); // layer=0 has no L<N> marker
    }

    #[tokio::test]
    async fn count_rows_at_layer_reports_correct_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        for i in 0..3 {
            let m = msg(&format!("c{i}"), "raw");
            idx.upsert_with_layer(&[(m, vec![0.1 * i as f32; 16], 0)])
                .await
                .unwrap();
        }
        for i in 0..2 {
            let m = msg(&format!("c{i}"), "span");
            idx.upsert_with_layer(&[(m, vec![0.7 * i as f32 + 0.1; 16], 2)])
                .await
                .unwrap();
        }
        assert_eq!(idx.count_rows_at_layer(0).await.unwrap(), 3);
        assert_eq!(idx.count_rows_at_layer(2).await.unwrap(), 2);
        assert_eq!(idx.count_rows_at_layer(1).await.unwrap(), 0);
    }

    #[test]
    fn parse_source_maps_rollup_sources_to_placeholder() {
        assert!(matches!(
            parse_source_or_placeholder("cc"),
            Source::ClaudeCode
        ));
        assert!(matches!(
            parse_source_or_placeholder("week"),
            Source::ClaudeCode
        ));
        assert!(matches!(
            parse_source_or_placeholder("month"),
            Source::ClaudeCode
        ));
        assert!(matches!(
            parse_source_or_placeholder("unknown-future"),
            Source::ClaudeCode
        ));
    }
}
