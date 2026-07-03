//! First-party exact-literal edit tool (issue #591, PR2). No regex —
//! ambiguity fails closed instead of guessing.

use std::path::{Path, PathBuf};

use mur_common::agent::FilesystemEntitlement;

use crate::llm::ToolDef;
use crate::tools::fs_policy::check_write_entitlement;
use crate::tools::{ToolError, ToolExecutor};

pub struct EditFileTool {
    pub working_dir: PathBuf,
    pub fs: FilesystemEntitlement,
}

impl EditFileTool {
    pub fn new(working_dir: PathBuf, fs: FilesystemEntitlement) -> Self {
        Self { working_dir, fs }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "edit_file".into(),
            description: "Replace an exact literal substring in a UTF-8 text file (no regex). By default the old_string must match exactly once — pass expected_count to replace N known occurrences. Edits are checked against the agent's filesystem write entitlements."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to edit (absolute, or relative to the agent working dir)" },
                    "old_string": { "type": "string", "description": "Exact literal text to replace" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "expected_count": { "type": "integer", "description": "Exact number of occurrences to replace; omit to require exactly one" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        let raw = input["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' field".into()))?;
        let old = input["old_string"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'old_string' field".into()))?;
        let new = input["new_string"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'new_string' field".into()))?;
        if old.is_empty() {
            return Err(ToolError::InvalidInput(
                "'old_string' must be non-empty".into(),
            ));
        }
        let joined = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            self.working_dir.join(raw)
        };
        let canonical = std::fs::canonicalize(&joined)
            .map_err(|e| ToolError::Execution(format!("cannot edit {}: {e}", joined.display())))?;
        check_write_entitlement(&self.fs, &canonical)?;

        let text = std::fs::read_to_string(&canonical).map_err(|e| {
            ToolError::Execution(format!("cannot read {}: {e}", canonical.display()))
        })?;
        let found = text.match_indices(old).count();
        let expected = input["expected_count"].as_i64().filter(|v| *v >= 1);
        match expected {
            Some(n) if found as i64 != n => {
                return Err(ToolError::InvalidInput(format!(
                    "expected {n} occurrence(s) of old_string, found {found}"
                )));
            }
            None if found == 0 => {
                return Err(ToolError::InvalidInput("old_string not found".into()));
            }
            None if found > 1 => {
                return Err(ToolError::InvalidInput(format!(
                    "ambiguous edit: old_string matches {found} times — pass expected_count or a longer anchor"
                )));
            }
            _ => {}
        }
        let updated = text.replace(old, new);
        std::fs::write(&canonical, &updated).map_err(|e| {
            ToolError::Execution(format!("cannot write {}: {e}", canonical.display()))
        })?;
        Ok(format!(
            "replaced {found} occurrence(s) in {}",
            canonical.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(td: &tempfile::TempDir) -> EditFileTool {
        let root = td.path().to_str().unwrap();
        EditFileTool::new(
            td.path().into(),
            FilesystemEntitlement {
                read: vec![],
                write: vec![root.to_string()],
                deny: vec![],
                ..Default::default()
            },
        )
    }

    fn seed(td: &tempfile::TempDir, content: &str) {
        std::fs::write(td.path().join("f.txt"), content).unwrap();
    }

    #[tokio::test]
    async fn single_match_replaces() {
        let td = tempfile::tempdir().unwrap();
        seed(&td, "aaa X bbb");
        tool(&td)
            .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y"}))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(td.path().join("f.txt")).unwrap(),
            "aaa Y bbb"
        );
    }

    #[tokio::test]
    async fn zero_matches_is_invalid_input() {
        let td = tempfile::tempdir().unwrap();
        seed(&td, "nothing here");
        let err = tool(&td)
            .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn multi_match_without_count_is_ambiguous() {
        let td = tempfile::tempdir().unwrap();
        seed(&td, "X and X");
        let err = tool(&td)
            .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[tokio::test]
    async fn expected_count_replaces_all_n() {
        let td = tempfile::tempdir().unwrap();
        seed(&td, "X and X");
        let r = tool(&td)
            .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y", "expected_count": 2}))
            .await
            .unwrap();
        assert!(r.contains("replaced 2"));
        assert_eq!(
            std::fs::read_to_string(td.path().join("f.txt")).unwrap(),
            "Y and Y"
        );
    }

    #[tokio::test]
    async fn expected_count_mismatch_rejected() {
        let td = tempfile::tempdir().unwrap();
        seed(&td, "X and X");
        let err = tool(&td)
            .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y", "expected_count": 3}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expected 3"));
    }
}
