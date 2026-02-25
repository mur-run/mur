//! YAML-based workflow store.
//!
//! Workflows are stored as individual YAML files in `~/.mur/workflows/`.
//! Mirrors `YamlStore` but for the `Workflow` type.

use anyhow::{Context, Result};
use mur_common::workflow::Workflow;
use std::fs;
use std::path::PathBuf;

/// The YAML workflow store.
pub struct WorkflowYamlStore {
    /// Root workflows directory (e.g. ~/.mur/workflows/)
    workflows_dir: PathBuf,
}

impl WorkflowYamlStore {
    /// Create a new WorkflowYamlStore pointing at the given directory.
    /// Creates the directory if it doesn't exist.
    pub fn new(workflows_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&workflows_dir)
            .with_context(|| format!("Failed to create workflows dir: {}", workflows_dir.display()))?;
        Ok(Self { workflows_dir })
    }

    /// Create a WorkflowYamlStore using the default ~/.mur/workflows/ path.
    pub fn default_store() -> Result<Self> {
        let dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".mur")
            .join("workflows");
        Self::new(dir)
    }

    /// List all workflow names (without .yaml extension).
    pub fn list_names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        if !self.workflows_dir.exists() {
            return Ok(names);
        }
        for entry in fs::read_dir(&self.workflows_dir)? {
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

    /// Load all workflows from disk.
    pub fn list_all(&self) -> Result<Vec<Workflow>> {
        let names = self.list_names()?;
        let mut workflows = Vec::with_capacity(names.len());
        for name in &names {
            match self.get(name) {
                Ok(w) => workflows.push(w),
                Err(e) => {
                    tracing::warn!("Skipping workflow {}: {}", name, e);
                }
            }
        }
        Ok(workflows)
    }

    /// Get a single workflow by name.
    pub fn get(&self, name: &str) -> Result<Workflow> {
        let path = self.workflow_path(name);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read workflow: {}", path.display()))?;
        let workflow: Workflow = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse workflow YAML: {}", path.display()))?;
        Ok(workflow)
    }

    /// Save a workflow to disk (atomic: write temp -> rename).
    pub fn save(&self, workflow: &Workflow) -> Result<()> {
        let path = self.workflow_path(&workflow.name);
        let yaml = serde_yaml::to_string(workflow)
            .with_context(|| format!("Failed to serialize workflow: {}", workflow.name))?;

        // Atomic write: temp file in same directory, then rename
        let tmp_path = path.with_extension("yaml.tmp");
        fs::write(&tmp_path, &yaml)
            .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("Failed to rename temp to final: {}", path.display()))?;

        Ok(())
    }

    /// Delete a workflow by name. Returns true if it existed.
    #[allow(dead_code)] // Public API
    pub fn delete(&self, name: &str) -> Result<bool> {
        let path = self.workflow_path(name);
        if path.exists() {
            fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Check if a workflow exists.
    pub fn exists(&self, name: &str) -> bool {
        self.workflow_path(name).exists()
    }

    /// Get the file path for a workflow name.
    fn workflow_path(&self, name: &str) -> PathBuf {
        self.workflows_dir.join(format!("{}.yaml", name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::Content;
    use mur_common::workflow::{Step, FailureAction};
    use tempfile::TempDir;

    fn make_test_workflow(name: &str) -> Workflow {
        Workflow {
            base: KnowledgeBase {
                name: name.to_string(),
                description: format!("Test workflow: {}", name),
                content: Content::Plain(format!("Steps for {}", name)),
                ..Default::default()
            },
            steps: vec![
                Step {
                    order: 1,
                    description: "First step".into(),
                    command: Some("echo hello".into()),
                    tool: Some("bash".into()),
                    needs_approval: false,
                    on_failure: FailureAction::Abort,
                },
            ],
            variables: vec![],
            source_sessions: vec![],
            trigger: String::new(),
            tools: vec!["bash".into()],
            published_version: 0,
            permission: Default::default(),
        }
    }

    #[test]
    fn test_save_and_load() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = WorkflowYamlStore::new(tmp.path().to_path_buf())?;

        let workflow = make_test_workflow("test-workflow");
        store.save(&workflow)?;

        let loaded = store.get("test-workflow")?;
        assert_eq!(loaded.name, "test-workflow");
        assert_eq!(loaded.steps.len(), 1);
        assert_eq!(loaded.steps[0].description, "First step");
        assert_eq!(loaded.tools, vec!["bash"]);

        Ok(())
    }

    #[test]
    fn test_list_names() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = WorkflowYamlStore::new(tmp.path().to_path_buf())?;

        store.save(&make_test_workflow("alpha"))?;
        store.save(&make_test_workflow("beta"))?;
        store.save(&make_test_workflow("gamma"))?;

        let names = store.list_names()?;
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);

        Ok(())
    }

    #[test]
    fn test_list_all() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = WorkflowYamlStore::new(tmp.path().to_path_buf())?;

        store.save(&make_test_workflow("wf-a"))?;
        store.save(&make_test_workflow("wf-b"))?;

        let all = store.list_all()?;
        assert_eq!(all.len(), 2);

        Ok(())
    }

    #[test]
    fn test_delete() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = WorkflowYamlStore::new(tmp.path().to_path_buf())?;

        store.save(&make_test_workflow("to-delete"))?;
        assert!(store.exists("to-delete"));

        let deleted = store.delete("to-delete")?;
        assert!(deleted);
        assert!(!store.exists("to-delete"));

        let deleted_again = store.delete("to-delete")?;
        assert!(!deleted_again);

        Ok(())
    }

    #[test]
    fn test_exists() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = WorkflowYamlStore::new(tmp.path().to_path_buf())?;

        assert!(!store.exists("nonexistent"));
        store.save(&make_test_workflow("exists-test"))?;
        assert!(store.exists("exists-test"));

        Ok(())
    }
}
