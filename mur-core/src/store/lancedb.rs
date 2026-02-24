//! LanceDB vector store for semantic search over patterns.
//!
//! YAML remains the source of truth. LanceDB is a rebuildable index.

use anyhow::{Context, Result};
use arrow_array::{Float32Array, RecordBatch, RecordBatchIterator, StringArray, FixedSizeListArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use mur_common::pattern::Pattern;
use std::path::Path;
use std::sync::Arc;

const TABLE_NAME: &str = "patterns";
const VECTOR_DIM: i32 = 768; // nomic-embed-text default

/// LanceDB-backed vector index for patterns.
pub struct VectorStore {
    db: lancedb::Connection,
}

impl VectorStore {
    /// Open or create the LanceDB database at the given path.
    pub async fn open(db_path: &Path) -> Result<Self> {
        let db = lancedb::connect(db_path.to_str().unwrap())
            .execute()
            .await
            .context("opening LanceDB")?;
        Ok(Self { db })
    }

    /// Build/rebuild the entire index from patterns + their embeddings.
    pub async fn build_index(
        &self,
        patterns: &[(Pattern, Vec<f32>)],
    ) -> Result<()> {
        // Drop existing table if any
        let tables = self.db.table_names().execute().await?;
        if tables.contains(&TABLE_NAME.to_string()) {
            self.db.drop_table(TABLE_NAME, &[]).await?;
        }

        if patterns.is_empty() {
            return Ok(());
        }

        let schema = Self::schema();

        let names: Vec<&str> = patterns.iter().map(|(p, _)| p.name.as_str()).collect();
        let descriptions: Vec<&str> = patterns.iter().map(|(p, _)| p.description.as_str()).collect();
        let contents: Vec<String> = patterns.iter().map(|(p, _)| p.content.as_text()).collect();
        let content_refs: Vec<&str> = contents.iter().map(|s| s.as_str()).collect();
        let tiers: Vec<String> = patterns
            .iter()
            .map(|(p, _)| format!("{:?}", p.tier).to_lowercase())
            .collect();
        let tier_refs: Vec<&str> = tiers.iter().map(|s| s.as_str()).collect();
        let importances: Vec<f32> = patterns.iter().map(|(p, _)| p.importance as f32).collect();

        // Build FixedSizeList for vectors
        let all_vectors: Vec<f32> = patterns.iter().flat_map(|(_, v)| v.clone()).collect();
        let values = Float32Array::from(all_vectors);
        let field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = FixedSizeListArray::new(field, VECTOR_DIM, Arc::new(values), None);

        let batch = RecordBatch::try_new(
            Arc::new(schema.clone()),
            vec![
                Arc::new(StringArray::from(names)),
                Arc::new(StringArray::from(descriptions)),
                Arc::new(StringArray::from(content_refs)),
                Arc::new(StringArray::from(tier_refs)),
                Arc::new(Float32Array::from(importances)),
                Arc::new(vector_array),
            ],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], Arc::new(schema));
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(batches);
        self.db
            .create_table(TABLE_NAME, reader)
            .execute()
            .await?;

        Ok(())
    }

    /// Search for similar patterns by embedding vector.
    pub async fn search(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&TABLE_NAME.to_string()) {
            return Ok(vec![]);
        }

        let table = self.db.open_table(TABLE_NAME).execute().await?;

        let results = table
            .vector_search(query_embedding)
            .context("vector search")?
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

            for i in 0..batch.num_rows() {
                search_results.push(SearchResult {
                    name: names.value(i).to_string(),
                    distance: distances.value(i),
                    similarity: 1.0 / (1.0 + distances.value(i)),
                });
            }
        }

        Ok(search_results)
    }

    fn schema() -> Schema {
        Schema::new(vec![
            Field::new("name", DataType::Utf8, false),
            Field::new("description", DataType::Utf8, false),
            Field::new("content", DataType::Utf8, false),
            Field::new("tier", DataType::Utf8, false),
            Field::new("importance", DataType::Float32, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    VECTOR_DIM,
                ),
                false,
            ),
        ])
    }
}

/// Result of a vector search.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub name: String,
    pub distance: f32,
    pub similarity: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::pattern::*;
    use tempfile::TempDir;

    fn make_pattern(name: &str) -> Pattern {
        Pattern {
            schema: 2,
            name: name.into(),
            description: format!("About {}", name),
            content: Content::Plain("test content".into()),
            tier: Tier::Session,
            importance: 0.5,
            confidence: 0.5,
            tags: Tags::default(),
            applies: Applies::default(),
            evidence: Evidence::default(),
            links: Links::default(),
            lifecycle: Lifecycle::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn random_embedding() -> Vec<f32> {
        (0..VECTOR_DIM as usize).map(|i| (i as f32 * 0.01).sin()).collect()
    }

    #[tokio::test]
    async fn test_build_and_search() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::open(tmp.path()).await.unwrap();

        let patterns = vec![
            (make_pattern("pattern-a"), random_embedding()),
            (make_pattern("pattern-b"), {
                let mut v = random_embedding();
                v[0] += 1.0;
                v
            }),
        ];

        store.build_index(&patterns).await.unwrap();

        let results = store.search(&random_embedding(), 5).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "pattern-a");
    }

    #[tokio::test]
    async fn test_empty_index() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::open(tmp.path()).await.unwrap();
        let results = store.search(&random_embedding(), 5).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_rebuild_index() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::open(tmp.path()).await.unwrap();

        let patterns = vec![(make_pattern("first"), random_embedding())];
        store.build_index(&patterns).await.unwrap();

        let patterns2 = vec![
            (make_pattern("second"), random_embedding()),
            (make_pattern("third"), {
                let mut v = random_embedding();
                v[0] += 0.5;
                v
            }),
        ];
        store.build_index(&patterns2).await.unwrap();

        let results = store.search(&random_embedding(), 10).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.name != "first"));
    }
}
