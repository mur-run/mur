//! First-party full-file write tool (issue #591, PR2).

use mur_common::agent::FilesystemEntitlement;

use crate::llm::ToolDef;
use crate::tools::fs_policy::{SessionCwd, check_write_entitlement};
use crate::tools::{ToolError, ToolExecutor};

pub struct WriteFileTool {
    pub session_cwd: SessionCwd,
    pub fs: FilesystemEntitlement,
}

impl WriteFileTool {
    pub fn new(session_cwd: SessionCwd, fs: FilesystemEntitlement) -> Self {
        Self { session_cwd, fs }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write_file".into(),
            description: "Create or fully overwrite a UTF-8 text file. Parent directories are NOT auto-created — create them explicitly first. Relative paths resolve against the shared session cwd (set by the bash tool's `cwd`); a `cd` inside a bash subprocess is NOT retained. Writes are checked against the agent's filesystem write entitlements."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Target file (absolute, or relative to the session cwd)" },
                    "content": { "type": "string", "description": "Full file contents to write" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        let raw = input["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' field".into()))?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'content' field".into()))?;
        let base = self.session_cwd.current();
        let joined = crate::tools::fs_policy::resolve_path(&base, raw);
        let parent = joined
            .parent()
            .ok_or_else(|| ToolError::InvalidInput("path has no parent directory".into()))?;
        let canonical_parent = std::fs::canonicalize(parent).map_err(|_| {
            ToolError::Execution(format!(
                "parent directory does not exist: {} (relative to session cwd {}) (create it explicitly first)",
                parent.display(),
                base.display()
            ))
        })?;
        let file_name = joined
            .file_name()
            .ok_or_else(|| ToolError::InvalidInput("path has no file name".into()))?;
        let target = canonical_parent.join(file_name);
        check_write_entitlement(&self.fs, &target)?;
        std::fs::write(&target, content).map_err(|e| {
            ToolError::Execution(format!(
                "cannot write {}: {e} (relative to session cwd {})",
                target.display(),
                base.display()
            ))
        })?;
        Ok(format!(
            "wrote {} bytes to {}",
            content.len(),
            target.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs_ent(write: &[&str], deny: &[&str]) -> FilesystemEntitlement {
        FilesystemEntitlement {
            read: vec![],
            write: write.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn creates_and_overwrites() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_str().unwrap();
        let t = WriteFileTool::new(SessionCwd::new(td.path().into()), fs_ent(&[root], &[]));
        let r = t
            .execute(serde_json::json!({"path": "a.txt", "content": "one"}))
            .await
            .unwrap();
        assert!(r.contains("wrote 3 bytes"));
        t.execute(serde_json::json!({"path": "a.txt", "content": "two2"}))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(td.path().join("a.txt")).unwrap(),
            "two2"
        );
    }

    #[tokio::test]
    async fn missing_parent_fails_loud() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_str().unwrap();
        let t = WriteFileTool::new(SessionCwd::new(td.path().into()), fs_ent(&[root], &[]));
        let err = t
            .execute(serde_json::json!({"path": "no/such/dir/a.txt", "content": "x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("parent directory does not exist"));
        // error must name the resolution base
        assert!(err.to_string().contains("session cwd"));
    }

    #[tokio::test]
    async fn write_requires_write_grant_and_deny_wins() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_str().unwrap();
        let none = WriteFileTool::new(SessionCwd::new(td.path().into()), fs_ent(&[], &[]));
        let err = none
            .execute(serde_json::json!({"path": "a.txt", "content": "x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not write-entitled"));
        let denied =
            WriteFileTool::new(SessionCwd::new(td.path().into()), fs_ent(&[root], &[root]));
        let err2 = denied
            .execute(serde_json::json!({"path": "a.txt", "content": "x"}))
            .await
            .unwrap_err();
        assert!(err2.to_string().contains("denied"));
    }
}
