//! First-party read-only file tool (issue #591, PR1).
//!
//! Complements the OS sandbox with an in-process entitlement check so a
//! disallowed read fails with a clear, policy-shaped error instead of a
//! kernel denial, and so file access is visible to per-tool policy instead
//! of hiding inside `bash`.

use std::path::{Path, PathBuf};

use mur_common::agent::FilesystemEntitlement;

use crate::llm::ToolDef;
use crate::tools::{ToolError, ToolExecutor};

/// Hard ceiling on returned bytes so a huge file cannot blow up the turn.
const MAX_RETURN_BYTES: usize = 512 * 1024;

pub struct ReadFileTool {
    /// Session cwd shared with `bash`; relative paths resolve against its
    /// current snapshot so `read_file rel/x` matches where `bash` last ran.
    pub session_cwd: crate::tools::fs_policy::SessionCwd,
    /// Filesystem grants from the agent profile; checked before every read.
    pub fs: FilesystemEntitlement,
}

impl ReadFileTool {
    pub fn new(
        session_cwd: crate::tools::fs_policy::SessionCwd,
        fs: FilesystemEntitlement,
    ) -> Self {
        Self { session_cwd, fs }
    }

    /// Prefix-match `path` against the entitlement lists after
    /// canonicalization. `deny` always wins; a read is allowed when the path
    /// falls under any `read` OR `write` grant (write implies read-back).
    fn check_entitlement(&self, canonical: &Path) -> Result<(), ToolError> {
        let under = |roots: &[String]| {
            roots.iter().any(|r| {
                let root = std::fs::canonicalize(r).unwrap_or_else(|_| PathBuf::from(r));
                canonical.starts_with(&root)
            })
        };
        if under(&self.fs.deny) {
            return Err(ToolError::Execution(format!(
                "path denied by entitlement: {}",
                canonical.display()
            )));
        }
        if under(&self.fs.read) || under(&self.fs.write) {
            return Ok(());
        }
        Err(ToolError::Execution(format!(
            "path not entitled: {} (grant it via `mur agent perm allow-read`)",
            canonical.display()
        )))
    }
}

#[async_trait::async_trait]
impl ToolExecutor for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file".into(),
            description: "Read a UTF-8 text file. Optional 1-indexed `offset`/`limit` select a line window. \
Relative paths resolve against the shared session working directory (the same base the `bash` tool uses, moved only by passing `bash` an explicit `cwd`); reads are checked against the agent's filesystem entitlements."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to read (absolute, or relative to the agent working dir)" },
                    "offset": { "type": "integer", "description": "1-indexed first line to return" },
                    "limit": { "type": "integer", "description": "Maximum number of lines to return" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        let raw = input["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' field".into()))?;
        let base = self.session_cwd.current();
        let joined = crate::tools::fs_policy::resolve_path(&base, raw);
        let canonical = std::fs::canonicalize(&joined).map_err(|e| {
            ToolError::Execution(crate::tools::fs_policy::format_io_error(
                "read", &joined, &base, &e,
            ))
        })?;
        self.check_entitlement(&canonical)?;

        let bytes = std::fs::read(&canonical).map_err(|e| {
            ToolError::Execution(crate::tools::fs_policy::format_io_error(
                "read", &canonical, &base, &e,
            ))
        })?;
        let text = String::from_utf8_lossy(&bytes);

        let offset = input["offset"].as_i64().filter(|v| *v >= 1);
        let limit = input["limit"].as_i64().filter(|v| *v >= 1);
        let windowed: String = match (offset, limit) {
            (None, None) => text.into_owned(),
            (o, l) => {
                let start = o.unwrap_or(1) as usize - 1;
                let take = l.map(|v| v as usize).unwrap_or(usize::MAX);
                text.lines()
                    .skip(start)
                    .take(take)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        if windowed.len() > MAX_RETURN_BYTES {
            let mut cut = windowed.into_bytes();
            cut.truncate(MAX_RETURN_BYTES);
            let mut s = String::from_utf8_lossy(&cut).into_owned();
            s.push_str("\n… [truncated at 512KiB — use offset/limit to window]");
            return Ok(s);
        }
        Ok(windowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs_ent(read: &[&str], write: &[&str], deny: &[&str]) -> FilesystemEntitlement {
        FilesystemEntitlement {
            read: read.iter().map(|s| s.to_string()).collect(),
            write: write.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn write_tmp(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    // runtime-file-tools-cwd: a shared SessionCwd handle means the bash tool's
    // explicit `cwd` moves the base that the file tools resolve against.
    #[tokio::test]
    async fn shared_cwd_bash_set_moves_read_file_base() {
        use crate::tools::bash::BashTool;
        use crate::tools::fs_policy::SessionCwd;

        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        // Seed the SAME relative filename in both dirs with distinct contents.
        write_tmp(home.path(), "spec.md", "HOME");
        write_tmp(other.path(), "spec.md", "OTHER");

        let shared = SessionCwd::new(home.path().into());
        let bash = BashTool::new(home.path().into(), shared.clone());
        let reader = ReadFileTool::new(
            shared.clone(),
            fs_ent(
                &[
                    home.path().to_str().unwrap(),
                    other.path().to_str().unwrap(),
                ],
                &[],
                &[],
            ),
        );

        // Before: relative read resolves against home.
        let before = reader
            .execute(serde_json::json!({"path": "spec.md"}))
            .await
            .unwrap();
        assert!(before.contains("HOME"), "expected HOME, got {before}");

        // bash with explicit cwd moves the shared base to `other`.
        bash.execute(serde_json::json!({"command": "true", "cwd": other.path().to_str().unwrap()}))
            .await
            .unwrap();

        // After: the SAME relative read now resolves against `other`.
        let after = reader
            .execute(serde_json::json!({"path": "spec.md"}))
            .await
            .unwrap();
        assert!(
            after.contains("OTHER"),
            "expected OTHER after bash cwd, got {after}"
        );
    }

    #[tokio::test]
    async fn missing_file_error_names_session_cwd() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[root], &[], &[]),
        );
        let err = t
            .execute(serde_json::json!({"path": "nope.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("relative to session cwd"));
    }

    #[tokio::test]
    async fn missing_path_is_invalid_input() {
        let td = tempfile::tempdir().unwrap();
        let t = ReadFileTool::new(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[], &[], &[]),
        );
        let err = t.execute(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn entitled_read_with_window() {
        let td = tempfile::tempdir().unwrap();
        write_tmp(td.path(), "f.txt", "l1\nl2\nl3\nl4");
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[root], &[], &[]),
        );
        let out = t
            .execute(serde_json::json!({"path": "f.txt", "offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert_eq!(out, "l2\nl3");
    }

    #[tokio::test]
    async fn write_grant_implies_read() {
        let td = tempfile::tempdir().unwrap();
        write_tmp(td.path(), "f.txt", "hi");
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[], &[root], &[]),
        );
        assert_eq!(
            t.execute(serde_json::json!({"path": "f.txt"}))
                .await
                .unwrap(),
            "hi"
        );
    }

    #[tokio::test]
    async fn deny_wins_over_read_grant() {
        let td = tempfile::tempdir().unwrap();
        write_tmp(td.path(), "f.txt", "secret");
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[root], &[], &[root]),
        );
        let err = t
            .execute(serde_json::json!({"path": "f.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("denied"));
    }

    #[tokio::test]
    async fn unentitled_path_is_rejected() {
        let td = tempfile::tempdir().unwrap();
        write_tmp(td.path(), "f.txt", "hi");
        let t = ReadFileTool::new(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&["/nonexistent-grant"], &[], &[]),
        );
        let err = t
            .execute(serde_json::json!({"path": "f.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not entitled"));
    }

    #[tokio::test]
    async fn nonexistent_file_is_execution_error() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[root], &[], &[]),
        );
        let err = t
            .execute(serde_json::json!({"path": "nope.txt"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
    }
}
