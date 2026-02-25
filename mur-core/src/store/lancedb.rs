//! LanceDB vector store for semantic search over patterns and workflows.
//!
//! YAML remains the source of truth. LanceDB is a rebuildable index.

use anyhow::{Context, Result};
use arrow_array::{Float32Array, RecordBatch, RecordBatchIterator, StringArray, FixedSizeListArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use mur_common::pattern::Pattern;
use mur_common::workflow::Workflow;
use std::path::Path;
use std::sync::Arc;

const TABLE_NAME: &str = "patterns";

/// LanceDB-backed vector index for patterns and workflows.
pub struct VectorStore {
    db: lancedb::Connection,
    dimensions: i32,
}

impl VectorStore {
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

        let schema = Self::schema(self.dimensions);

        let names: Vec<&str> = patterns.iter().map(|(p, _)| p.name.as_str()).collect();
        let descriptions: Vec<&str> = patterns.iter().map(|(p, _)| p.description.as_str()).collect();
        let contents: Vec<String> = patterns.iter().map(|(p, _)| content_with_attachment_descriptions(p)).collect();
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
        self.db
            .create_table(TABLE_NAME, reader)
            .execute()
            .await?;

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
        let mut descriptions: Vec<String> = patterns.iter().map(|(p, _)| p.description.clone()).collect();
        let mut contents: Vec<String> = patterns.iter().map(|(p, _)| content_with_attachment_descriptions(p)).collect();
        let mut tiers: Vec<String> = patterns.iter().map(|(p, _)| format!("{:?}", p.tier).to_lowercase()).collect();
        let mut importances: Vec<f32> = patterns.iter().map(|(p, _)| p.importance as f32).collect();
        let mut item_types: Vec<String> = vec!["pattern".into(); patterns.len()];
        let mut all_vectors: Vec<f32> = patterns.iter().flat_map(|(_, v)| v.clone()).collect();

        // Append fields from workflows
        for (w, v) in workflows {
            names.push(w.name.clone());
            descriptions.push(w.description.clone());
            contents.push(w.content.as_text());
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
        self.db
            .create_table(TABLE_NAME, reader)
            .execute()
            .await?;

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
}

/// Build the content string for indexing, including attachment descriptions.
fn content_with_attachment_descriptions(pattern: &Pattern) -> String {
    let mut text = pattern.content.as_text();
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

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::pattern::*;
    use tempfile::TempDir;

    const TEST_DIM: i32 = 64;

    fn make_pattern(name: &str) -> Pattern {
        Pattern {
            base: mur_common::knowledge::KnowledgeBase {
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
                ..Default::default()
            },
            attachments: vec![],
        }
    }

    fn make_workflow(name: &str) -> Workflow {
        Workflow {
            base: mur_common::knowledge::KnowledgeBase {
                name: name.into(),
                description: format!("Workflow: {}", name),
                content: Content::Plain("workflow content".into()),
                ..Default::default()
            },
            steps: vec![],
            variables: vec![],
            source_sessions: vec![],
            trigger: String::new(),
            tools: vec![],
            published_version: 0,
            permission: Default::default(),
        }
    }

    fn random_embedding() -> Vec<f32> {
        (0..TEST_DIM as usize).map(|i| (i as f32 * 0.01).sin()).collect()
    }

    #[tokio::test]
    async fn test_build_and_search() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::open(tmp.path(), TEST_DIM).await.unwrap();

        let patterns = vec![
            (make_pattern("pattern-a"), random_embedding()),
            (make_pattern("pattern-b"), {
                let mut v = random_embedding();
                v[0] += 1.0;
                v
            }),
        ];

        store.build_index(&patterns).await.unwrap();

        let results = store.search(&random_embedding(), 5, None).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "pattern-a");
        assert_eq!(results[0].item_type, "pattern");
    }

    #[tokio::test]
    async fn test_empty_index() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::open(tmp.path(), TEST_DIM).await.unwrap();
        let results = store.search(&random_embedding(), 5, None).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_rebuild_index() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::open(tmp.path(), TEST_DIM).await.unwrap();

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

        let results = store.search(&random_embedding(), 10, None).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.name != "first"));
    }

    #[test]
    fn test_content_with_attachment_descriptions() {
        let mut p = make_pattern("attach-test");
        assert_eq!(
            super::content_with_attachment_descriptions(&p),
            "test content"
        );

        // Add attachments with descriptions
        p.attachments = vec![
            mur_common::pattern::Attachment {
                att_type: mur_common::pattern::AttachmentType::Diagram,
                format: mur_common::pattern::AttachmentFormat::Mermaid,
                path: "attach-test/arch.mermaid".into(),
                description: "System architecture overview".into(),
            },
            mur_common::pattern::Attachment {
                att_type: mur_common::pattern::AttachmentType::Image,
                format: mur_common::pattern::AttachmentFormat::Png,
                path: "attach-test/screen.png".into(),
                description: "Dashboard screenshot".into(),
            },
        ];

        let text = super::content_with_attachment_descriptions(&p);
        assert!(text.contains("test content"));
        assert!(text.contains("System architecture overview"));
        assert!(text.contains("Dashboard screenshot"));
    }

    #[test]
    fn test_content_with_empty_attachment_descriptions() {
        let mut p = make_pattern("empty-desc");
        p.attachments = vec![mur_common::pattern::Attachment {
            att_type: mur_common::pattern::AttachmentType::Diagram,
            format: mur_common::pattern::AttachmentFormat::Mermaid,
            path: "empty-desc/flow.mermaid".into(),
            description: "".into(), // empty description
        }];

        let text = super::content_with_attachment_descriptions(&p);
        // Should not add extra newlines for empty descriptions
        assert_eq!(text, "test content");
    }

    #[tokio::test]
    async fn test_unified_index() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::open(tmp.path(), TEST_DIM).await.unwrap();

        let patterns = vec![(make_pattern("pat-a"), random_embedding())];
        let workflows = vec![(make_workflow("wf-a"), {
            let mut v = random_embedding();
            v[0] += 1.0;
            v
        })];

        store.build_unified_index(&patterns, &workflows).await.unwrap();

        // Search all
        let results = store.search(&random_embedding(), 10, None).await.unwrap();
        assert_eq!(results.len(), 2);

        // Filter to patterns only
        let pat_results = store.search(&random_embedding(), 10, Some("pattern")).await.unwrap();
        assert!(pat_results.iter().all(|r| r.item_type == "pattern"));

        // Filter to workflows only
        let wf_results = store.search(&random_embedding(), 10, Some("workflow")).await.unwrap();
        assert!(wf_results.iter().all(|r| r.item_type == "workflow"));
    }
}
