//! YAML-based pipeline store.
//!
//! Pipelines are stored as individual YAML files in `~/.mur/pipelines/`.
//! Mirrors `WorkflowYamlStore` but for the `PipelineDef` type.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// A stored pipeline definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineDef {
    pub id: String,
    pub expression: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "chrono::Utc::now")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default = "chrono::Utc::now")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// The YAML pipeline store.
#[derive(Clone)]
pub struct PipelineYamlStore {
    pipelines_dir: PathBuf,
}

impl PipelineYamlStore {
    /// Create a new PipelineYamlStore pointing at the given directory.
    /// Creates the directory if it doesn't exist.
    pub fn new(pipelines_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&pipelines_dir).with_context(|| {
            format!(
                "Failed to create pipelines dir: {}",
                pipelines_dir.display()
            )
        })?;
        Ok(Self { pipelines_dir })
    }

    /// List all pipeline IDs (without .yaml extension).
    pub fn list_names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        if !self.pipelines_dir.exists() {
            return Ok(names);
        }
        for entry in fs::read_dir(&self.pipelines_dir)? {
            let entry = entry?;
            let path = entry.path();
            if (path.extension().and_then(|e| e.to_str()) == Some("yaml")
                || path.extension().and_then(|e| e.to_str()) == Some("yml"))
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Load all pipelines from disk.
    pub fn list_all(&self) -> Result<Vec<PipelineDef>> {
        let names = self.list_names()?;
        let mut pipelines = Vec::with_capacity(names.len());
        for name in &names {
            match self.get(name) {
                Ok(p) => pipelines.push(p),
                Err(e) => {
                    tracing::warn!("Skipping pipeline {}: {}", name, e);
                }
            }
        }
        Ok(pipelines)
    }

    /// Get a single pipeline by ID.
    pub fn get(&self, id: &str) -> Result<PipelineDef> {
        let path = self.pipeline_path(id);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read pipeline: {}", path.display()))?;
        let pipeline: PipelineDef = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse pipeline YAML: {}", path.display()))?;
        Ok(pipeline)
    }

    /// Save a pipeline to disk (atomic: write temp -> rename).
    pub fn save(&self, pipeline: &PipelineDef) -> Result<()> {
        let path = self.pipeline_path(&pipeline.id);
        let yaml = serde_yaml::to_string(pipeline)
            .with_context(|| format!("Failed to serialize pipeline: {}", pipeline.id))?;

        let tmp_path = path.with_extension("yaml.tmp");
        fs::write(&tmp_path, &yaml)
            .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("Failed to rename temp to final: {}", path.display()))?;

        Ok(())
    }

    /// Delete a pipeline by ID. Returns true if it existed.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let path = self.pipeline_path(id);
        if path.exists() {
            fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Check if a pipeline exists.
    pub fn exists(&self, id: &str) -> bool {
        self.pipeline_path(id).exists()
    }

    /// Get the file path for a pipeline ID.
    fn pipeline_path(&self, id: &str) -> PathBuf {
        self.pipelines_dir.join(format!("{}.yaml", id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_test_pipeline(id: &str) -> PipelineDef {
        PipelineDef {
            id: id.to_string(),
            expression: "w1 | w2".to_string(),
            description: format!("Test pipeline: {}", id),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_save_and_load() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = PipelineYamlStore::new(tmp.path().to_path_buf())?;

        let pipeline = make_test_pipeline("test-pipeline");
        store.save(&pipeline)?;

        let loaded = store.get("test-pipeline")?;
        assert_eq!(loaded.id, "test-pipeline");
        assert_eq!(loaded.expression, "w1 | w2");

        Ok(())
    }

    #[test]
    fn test_list_names() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = PipelineYamlStore::new(tmp.path().to_path_buf())?;

        store.save(&make_test_pipeline("alpha"))?;
        store.save(&make_test_pipeline("beta"))?;

        let names = store.list_names()?;
        assert_eq!(names, vec!["alpha", "beta"]);

        Ok(())
    }

    #[test]
    fn test_delete() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = PipelineYamlStore::new(tmp.path().to_path_buf())?;

        store.save(&make_test_pipeline("to-delete"))?;
        assert!(store.exists("to-delete"));

        let deleted = store.delete("to-delete")?;
        assert!(deleted);
        assert!(!store.exists("to-delete"));

        let deleted_again = store.delete("to-delete")?;
        assert!(!deleted_again);

        Ok(())
    }
}
