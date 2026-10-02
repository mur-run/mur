//! LanceDB-backed implementation of the `VectorStore` trait.
//!
//! YAML remains the source of truth. LanceDB is a rebuildable index.

use anyhow::{Context, Result};
use arrow_array::{
    FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use mur_common::pattern::Pattern;
use mur_common::workflow::Workflow;
use std::path::Path;
use std::sync::Arc;

const TABLE_NAME: &str = "patterns";

/// Name of the LanceDB table that stores source chunks (separate from `patterns`).
pub const SOURCES_TABLE: &str = "sources";

/// Arrow schema for the sources table.
pub fn sources_schema(dimensions: i32) -> Schema {
    Schema::new(vec![
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("source_id", DataType::Utf8, false),
        Field::new("external_id", DataType::Utf8, false),
        Field::new("ordinal", DataType::UInt64, false),
        Field::new("text", DataType::Utf8, false),
        Field::new("heading_path", DataType::Utf8, false), // JSON-encoded array
        Field::new("char_start", DataType::UInt64, false),
        Field::new("char_end", DataType::UInt64, false),
        Field::new("updated_at_ms", DataType::Int64, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dimensions,
            ),
            false,
        ),
    ])
}

/// LanceDB-backed vector index for patterns and workflows.
pub struct LanceDbStore {
    db: lancedb::Connection,
    dimensions: i32,
}

impl LanceDbStore {
    /// Open or create the LanceDB database at the given path.
    pub async fn open(db_path: &Path, dimensions: i32) -> Result<Self> {
        let db = lancedb::connect(db_path.to_str().unwrap())
            .execute()
            .await
            .context("opening LanceDB")?;
        Ok(Self { db, dimensions })
    }

    /// Build/rebuild the entire index from patterns + their embeddings.
    #[allow(dead_code)] // Public API, used by tests
    pub async fn build_index(&self, patterns: &[(Pattern, Vec<f32>)]) -> Result<()> {
        // Drop existing table if any
        let tables = self.db.table_names().execute().await?;
        if tables.contains(&TABLE_NAME.to_string()) {
            self.db.drop_table(TABLE_NAME, &[]).await?;
        }

        if patterns.is_empty() {
            return Ok(());
        }

        let schema = Self::schema(self.dimensions);

        let names: Vec<&str> = patterns.iter().map(|(p, _)| p.name.as_str()).collect();
        let descriptions: Vec<&str> = patterns
            .iter()
            .map(|(p, _)| p.description.as_str())
            .collect();
        let contents: Vec<String> = patterns
            .iter()
            .map(|(p, _)| content_with_attachment_descriptions(p))
            .collect();
        let content_refs: Vec<&str> = contents.iter().map(|s| s.as_str()).collect();
        let tiers: Vec<String> = patterns
            .iter()
            .map(|(p, _)| format!("{:?}", p.tier).to_lowercase())
            .collect();
        let tier_refs: Vec<&str> = tiers.iter().map(|s| s.as_str()).collect();
        let importances: Vec<f32> = patterns.iter().map(|(p, _)| p.importance as f32).collect();
        let item_types: Vec<&str> = vec!["pattern"; patterns.len()];

        // Build FixedSizeList for vectors
        let all_vectors: Vec<f32> = patterns.iter().flat_map(|(_, v)| v.clone()).collect();
        let values = Float32Array::from(all_vectors);
        let field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = FixedSizeListArray::new(field, self.dimensions, Arc::new(values), None);

        let batch = RecordBatch::try_new(
            Arc::new(schema.clone()),
            vec![
                Arc::new(StringArray::from(names)),
                Arc::new(StringArray::from(descriptions)),
                Arc::new(StringArray::from(content_refs)),
                Arc::new(StringArray::from(tier_refs)),
                Arc::new(Float32Array::from(importances)),
                Arc::new(StringArray::from(item_types)),
                Arc::new(vector_array),
            ],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], Arc::new(schema));
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(batches);
        self.db.create_table(TABLE_NAME, reader).execute().await?;

        Ok(())
    }

    /// Build/rebuild a unified index from patterns AND workflows with their embeddings.
    pub async fn build_unified_index(
        &self,
        patterns: &[(Pattern, Vec<f32>)],
        workflows: &[(Workflow, Vec<f32>)],
    ) -> Result<()> {
        // Drop existing table if any
        let tables = self.db.table_names().execute().await?;
        if tables.contains(&TABLE_NAME.to_string()) {
            self.db.drop_table(TABLE_NAME, &[]).await?;
        }

        let total = patterns.len() + workflows.len();
        if total == 0 {
            return Ok(());
        }

        let schema = Self::schema(self.dimensions);

        // Collect fields from patterns
        let mut names: Vec<String> = patterns.iter().map(|(p, _)| p.name.clone()).collect();
        let mut descriptions: Vec<String> = patterns
            .iter()
            .map(|(p, _)| p.description.clone())
            .collect();
        let mut contents: Vec<String> = patterns
            .iter()
            .map(|(p, _)| content_with_attachment_descriptions(p))
            .collect();
        let mut tiers: Vec<String> = patterns
            .iter()
            .map(|(p, _)| format!("{:?}", p.tier).to_lowercase())
            .collect();
        let mut importances: Vec<f32> = patterns.iter().map(|(p, _)| p.importance as f32).collect();
        let mut item_types: Vec<String> = vec!["pattern".into(); patterns.len()];
        let mut all_vectors: Vec<f32> = patterns.iter().flat_map(|(_, v)| v.clone()).collect();

        // Append fields from workflows
        for (w, v) in workflows {
            names.push(w.name.clone());
            descriptions.push(w.description.clone());
            contents.push(w.content.as_text().into_owned());
            tiers.push(format!("{:?}", w.tier).to_lowercase());
            importances.push(w.importance as f32);
            item_types.push("workflow".into());
            all_vectors.extend(v.iter());
        }

        let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let desc_refs: Vec<&str> = descriptions.iter().map(|s| s.as_str()).collect();
        let content_refs: Vec<&str> = contents.iter().map(|s| s.as_str()).collect();
        let tier_refs: Vec<&str> = tiers.iter().map(|s| s.as_str()).collect();
        let type_refs: Vec<&str> = item_types.iter().map(|s| s.as_str()).collect();

        let values = Float32Array::from(all_vectors);
        let field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = FixedSizeListArray::new(field, self.dimensions, Arc::new(values), None);

        let batch = RecordBatch::try_new(
            Arc::new(schema.clone()),
            vec![
                Arc::new(StringArray::from(name_refs)),
                Arc::new(StringArray::from(desc_refs)),
                Arc::new(StringArray::from(content_refs)),
                Arc::new(StringArray::from(tier_refs)),
                Arc::new(Float32Array::from(importances)),
                Arc::new(StringArray::from(type_refs)),
                Arc::new(vector_array),
            ],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], Arc::new(schema));
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(batches);
        self.db.create_table(TABLE_NAME, reader).execute().await?;

        Ok(())
    }

    /// Search for similar items by embedding vector.
    /// Optionally filter by item_type ("pattern" or "workflow").
    pub async fn search(
        &self,
        query_embedding: &[f32],
        limit: usize,
        item_type: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&TABLE_NAME.to_string()) {
            return Ok(vec![]);
        }

        let table = self.db.open_table(TABLE_NAME).execute().await?;

        let mut query = table
            .vector_search(query_embedding)
            .context("vector search")?;

        if let Some(t) = item_type {
            query = query.only_if(format!("item_type = '{}'", t));
        }

        let results = query
            .limit(limit)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;

        let mut search_results = Vec::new();
        for batch in &results {
            let names = batch
                .column_by_name("name")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let distances = batch
                .column_by_name("_distance")
                .unwrap()
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap();
            let types = batch
                .column_by_name("item_type")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                search_results.push(SearchResult {
                    name: names.value(i).to_string(),
                    distance: distances.value(i),
                    similarity: 1.0 / (1.0 + distances.value(i)),
                    item_type: types
                        .map(|t| t.value(i).to_string())
                        .unwrap_or_else(|| "pattern".into()),
                });
            }
        }

        Ok(search_results)
    }

    fn schema(dimensions: i32) -> Schema {
        Schema::new(vec![
            Field::new("name", DataType::Utf8, false),
            Field::new("description", DataType::Utf8, false),
            Field::new("content", DataType::Utf8, false),
            Field::new("tier", DataType::Utf8, false),
            Field::new("importance", DataType::Float32, false),
            Field::new("item_type", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dimensions,
                ),
                false,
            ),
        ])
    }

    /// Create the `sources` table if it doesn't exist. Idempotent.
    pub async fn ensure_sources_table(&self) -> Result<()> {
        let tables = self.db.table_names().execute().await?;
        if tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(());
        }
        let schema = sources_schema(self.dimensions);
        let empty: Vec<std::result::Result<RecordBatch, arrow_schema::ArrowError>> = Vec::new();
        let reader = RecordBatchIterator::new(empty, Arc::new(schema));
        self.db
            .create_table(
                SOURCES_TABLE,
                Box::new(reader) as Box<dyn arrow_array::RecordBatchReader + Send>,
            )
            .execute()
            .await
            .context("creating sources table")?;
        Ok(())
    }
}

/// Build the content string for indexing, including attachment descriptions.
fn content_with_attachment_descriptions(pattern: &Pattern) -> String {
    let mut text = pattern.content.as_text().into_owned();
    for att in &pattern.attachments {
        if !att.description.is_empty() {
            text.push_str("\n\n");
            text.push_str(&att.description);
        }
    }
    text
}

/// Result of a vector search.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub name: String,
    #[allow(dead_code)] // Exposed for callers that need raw distance
    pub distance: f32,
    pub similarity: f32,
    #[allow(dead_code)] // Public API for callers that filter by type
    pub item_type: String,
}

use super::{EmbeddedChunk, Hit, SearchFilter, VectorStore};
use async_trait::async_trait;

#[async_trait]
impl VectorStore for LanceDbStore {
    async fn upsert(&self, chunks: &[EmbeddedChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        // Validate dimensions BEFORE touching the table. The upsert is
        // delete-then-add; if the add fails on a dimension mismatch after the
        // delete succeeded, the rows are silently lost.
        let want = self.dimensions as usize;
        if let Some(bad) = chunks.iter().find(|c| c.embedding.len() != want) {
            anyhow::bail!(
                "embedding for chunk '{}' has {} dimensions, store expects {}",
                bad.chunk_id,
                bad.embedding.len(),
                want
            );
        }
        self.ensure_sources_table().await?;
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        let existing = table.schema().await?;
        if let Ok(field) = existing.field_with_name("vector")
            && let DataType::FixedSizeList(_, n) = field.data_type()
            && *n != self.dimensions
        {
            anyhow::bail!(
                "sources table has {n}-dimensional vectors but the configured embedding \
                 dimension is {}; move the table aside and run `mur skill reindex-vec` \
                 to rebuild it",
                self.dimensions
            );
        }

        // Delete any existing rows with these chunk_ids (idempotent upsert).
        let ids: Vec<String> = chunks
            .iter()
            .map(|c| format!("'{}'", c.chunk_id.replace('\'', "''")))
            .collect();
        let predicate = format!("chunk_id IN ({})", ids.join(","));
        let _ = table.delete(&predicate).await;

        // Build column arrays.
        let chunk_ids: Vec<&str> = chunks.iter().map(|c| c.chunk_id.as_str()).collect();
        let source_ids: Vec<&str> = chunks.iter().map(|c| c.source_id.as_str()).collect();
        let external_ids: Vec<&str> = chunks.iter().map(|c| c.external_id.as_str()).collect();
        let ordinals: Vec<u64> = chunks.iter().map(|c| c.ordinal as u64).collect();
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let heading_paths: Vec<String> = chunks
            .iter()
            .map(|c| serde_json::to_string(&c.heading_path).unwrap_or_else(|_| "[]".into()))
            .collect();
        let heading_path_refs: Vec<&str> = heading_paths.iter().map(|s| s.as_str()).collect();
        let char_starts: Vec<u64> = chunks.iter().map(|c| c.char_range.0 as u64).collect();
        let char_ends: Vec<u64> = chunks.iter().map(|c| c.char_range.1 as u64).collect();
        let updated_at_ms: Vec<i64> = chunks
            .iter()
            .map(|c| c.updated_at.timestamp_millis())
            .collect();

        let all_vectors: Vec<f32> = chunks.iter().flat_map(|c| c.embedding.clone()).collect();
        let values = Float32Array::from(all_vectors);
        let item_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array =
            FixedSizeListArray::new(item_field, self.dimensions, Arc::new(values), None);

        let schema = sources_schema(self.dimensions);
        use arrow_array::{Int64Array, UInt64Array};
        let batch = RecordBatch::try_new(
            Arc::new(schema.clone()),
            vec![
                Arc::new(StringArray::from(chunk_ids)),
                Arc::new(StringArray::from(source_ids)),
                Arc::new(StringArray::from(external_ids)),
                Arc::new(UInt64Array::from(ordinals)),
                Arc::new(StringArray::from(texts)),
                Arc::new(StringArray::from(heading_path_refs)),
                Arc::new(UInt64Array::from(char_starts)),
                Arc::new(UInt64Array::from(char_ends)),
                Arc::new(Int64Array::from(updated_at_ms)),
                Arc::new(vector_array),
            ],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], Arc::new(schema));
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(batches);
        table.add(reader).execute().await?;
        Ok(())
    }

    async fn search(&self, query_vec: &[f32], k: usize, filter: &SearchFilter) -> Result<Vec<Hit>> {
        use futures::TryStreamExt;
        use lancedb::query::{ExecutableQuery, QueryBase};

        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(vec![]);
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;

        let mut query = table
            .vector_search(query_vec.to_vec())
            .context("vector_search")?;

        // Build WHERE predicate from filter.
        let mut predicates: Vec<String> = Vec::new();
        if let Some(ids) = &filter.source_ids
            && !ids.is_empty()
        {
            let escaped: Vec<String> = ids
                .iter()
                .map(|s| format!("'{}'", s.replace('\'', "''")))
                .collect();
            predicates.push(format!("source_id IN ({})", escaped.join(",")));
        }
        if let Some(since) = filter.since {
            predicates.push(format!("updated_at_ms >= {}", since.timestamp_millis()));
        }
        if !predicates.is_empty() {
            query = query.only_if(predicates.join(" AND "));
        }

        let results = query
            .limit(k)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;

        let mut hits: Vec<Hit> = Vec::new();
        for batch in &results {
            use arrow_array::{Int64Array, StringArray};
            let chunk_ids = batch
                .column_by_name("chunk_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column chunk_id missing or wrong type")?;
            let source_ids = batch
                .column_by_name("source_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column source_id missing")?;
            let external_ids = batch
                .column_by_name("external_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column external_id missing")?;
            let texts = batch
                .column_by_name("text")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column text missing")?;
            let heading_paths = batch
                .column_by_name("heading_path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column heading_path missing")?;
            let updated_at_ms = batch
                .column_by_name("updated_at_ms")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .context("column updated_at_ms missing")?;
            let distances = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                .context("column _distance missing")?;

            for i in 0..batch.num_rows() {
                let d = distances.value(i);
                let score = 1.0 / (1.0 + d);
                let hp: Vec<String> =
                    serde_json::from_str(heading_paths.value(i)).unwrap_or_default();
                let ms = updated_at_ms.value(i);
                let ts = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                    .unwrap_or_else(chrono::Utc::now);
                hits.push(Hit {
                    chunk_id: chunk_ids.value(i).to_string(),
                    source_id: source_ids.value(i).to_string(),
                    external_id: external_ids.value(i).to_string(),
                    score,
                    text: texts.value(i).to_string(),
                    heading_path: hp,
                    updated_at: ts,
                });
            }
        }
        Ok(hits)
    }

    async fn delete_by_external_ids(&self, source_id: &str, external_ids: &[String]) -> Result<()> {
        if external_ids.is_empty() {
            return Ok(());
        }
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(());
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        let escaped: Vec<String> = external_ids
            .iter()
            .map(|e| format!("'{}'", e.replace('\'', "''")))
            .collect();
        let predicate = format!(
            "source_id = '{}' AND external_id IN ({})",
            source_id.replace('\'', "''"),
            escaped.join(",")
        );
        table.delete(&predicate).await?;
        Ok(())
    }

    async fn delete_by_source(&self, source_id: &str) -> Result<()> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(());
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        let predicate = format!("source_id = '{}'", source_id.replace('\'', "''"));
        table.delete(&predicate).await?;
        Ok(())
    }

    async fn list_external_ids(&self, source_id: &str) -> Result<Vec<String>> {
        use futures::TryStreamExt;
        use lancedb::query::{ExecutableQuery, QueryBase};

        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(vec![]);
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        let batches = table
            .query()
            .only_if(format!("source_id = '{}'", source_id.replace('\'', "''")))
            .select(lancedb::query::Select::Columns(vec![
                "external_id".to_string(),
            ]))
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;

        let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
        for batch in &batches {
            let col = batch
                .column_by_name("external_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("column external_id missing")?;
            for i in 0..batch.num_rows() {
                set.insert(col.value(i).to_string());
            }
        }
        Ok(set.into_iter().collect())
    }

    async fn count(&self, source_id: Option<&str>) -> Result<usize> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&SOURCES_TABLE.to_string()) {
            return Ok(0);
        }
        let table = self.db.open_table(SOURCES_TABLE).execute().await?;
        let total = match source_id {
            None => table.count_rows(None).await?,
            Some(sid) => {
                table
                    .count_rows(Some(format!("source_id = '{}'", sid.replace('\'', "''"))))
                    .await?
            }
        };
        Ok(total)
    }

    async fn rebuild_index(&self) -> Result<()> {
        anyhow::bail!("LanceDbStore::rebuild_index (trait) is a stub until P1.3 orchestrates")
    }
}

// Compile-time assertion that LanceDbStore implements VectorStore.
const _: () = {
    fn _assert_impl<T: VectorStore>() {}
    fn _check() {
        _assert_impl::<LanceDbStore>();
    }
};

#[cfg(test)]
mod tests;
