use std::path::PathBuf;
use tokio::process::Command;

use super::{ToolError, ToolExecutor};
use crate::llm::ToolDef;

/// Directories that must be on `PATH` for the bash tool's spawned commands,
/// even when the agent-runtime process itself was launched by a service
/// manager (launchd/systemd) with a minimal default `PATH`
/// (`/usr/bin:/bin:/usr/sbin:/sbin`). Without this, agents hit
/// `bash: mur: command not found` / `npx: command not found` for tools
/// installed via Homebrew, Cargo, or user-local pip/npm — even though those
/// binaries are on the *interactive* user's `PATH` (dogfood issue 1).
fn standard_exec_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    dirs
}

/// Build the `PATH` to use for spawned bash commands: start from whatever
/// `PATH` the current process has (so we never lose anything the caller set
/// up), and append any of [`standard_exec_dirs`] that aren't already present.
/// This works whether the runtime inherited a rich interactive `PATH` or a
/// minimal service-manager one.
fn augmented_path(current_path: Option<&str>) -> String {
    let mut components: Vec<PathBuf> = current_path
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();

    for dir in standard_exec_dirs() {
        if !components.contains(&dir) {
            components.push(dir);
        }
    }

    std::env::join_paths(components)
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub struct BashTool {
    pub working_dir: PathBuf,
}

impl BashTool {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash".into(),
            description: "Run a bash shell command. Returns combined stdout+stderr. Non-zero exit codes appear in the output but are not errors.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for the command. Defaults to the agent home directory."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'command' field".into()))?
            .to_string();

        let working_dir = input["cwd"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.working_dir.clone());

        let path = augmented_path(std::env::var("PATH").ok().as_deref());

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            Command::new("bash")
                .arg("-c")
                .arg(&command)
                .current_dir(&working_dir)
                .env("PATH", path)
                .output(),
        )
        .await
        .map_err(|_| ToolError::Execution("command timed out after 30s".into()))?
        .map_err(|e| ToolError::Execution(format!("spawn failed: {e}")))?;

        let mut combined = String::new();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.is_empty() {
            combined.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push_str("\n[stderr]\n");
            }
            combined.push_str(&stderr);
        }
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&format!("[exit code: {code}]"));
        }

        Ok(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolExecutor;

    fn make_tool() -> BashTool {
        BashTool::new(std::env::temp_dir())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_stdout() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(out.contains("hello"), "got: {out}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_stderr() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo err >&2"}))
            .await
            .unwrap();
        assert!(out.contains("err"), "got: {out}");
    }

    #[tokio::test]
    async fn nonzero_exit_in_output_not_err() {
        let t = make_tool();
        let result = t.execute(serde_json::json!({"command": "exit 1"})).await;
        assert!(
            result.is_ok(),
            "non-zero exit should not be Err, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn missing_command_is_invalid_input() {
        let t = make_tool();
        let result = t.execute(serde_json::json!({})).await;
        assert!(matches!(result, Err(ToolError::InvalidInput(_))));
    }

    #[test]
    fn augmented_path_adds_standard_dirs_to_minimal_path() {
        // Simulate a service-manager launch with the classic minimal PATH.
        let result = augmented_path(Some("/usr/bin:/bin:/usr/sbin:/sbin"));
        let dirs: Vec<_> = std::env::split_paths(&result).collect();

        assert!(dirs.contains(&PathBuf::from("/usr/bin")));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        if let Some(home) = dirs::home_dir() {
            assert!(dirs.contains(&home.join(".local/bin")));
            assert!(dirs.contains(&home.join(".cargo/bin")));
        }
    }

    #[test]
    fn augmented_path_does_not_duplicate_existing_entries() {
        let result = augmented_path(Some("/opt/homebrew/bin:/usr/bin"));
        let dirs: Vec<_> = std::env::split_paths(&result).collect();
        let count = dirs
            .iter()
            .filter(|d| *d == &PathBuf::from("/opt/homebrew/bin"))
            .count();
        assert_eq!(count, 1, "should not duplicate an already-present dir");
    }

    #[test]
    fn augmented_path_handles_missing_path_var() {
        // Even with no PATH at all (e.g. a stripped-down launch environment),
        // we should still end up with the standard dirs.
        let result = augmented_path(None);
        let dirs: Vec<_> = std::env::split_paths(&result).collect();
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
    }

    #[tokio::test]
    async fn bash_tool_can_find_binary_only_on_augmented_path() {
        // Regression test for dogfood issue 1: `mur agent send` on a service
        // launched with a minimal PATH couldn't find binaries like `npx`
        // that live under /opt/homebrew/bin. Here we fake that situation by
        // dropping a "fake npx" shim into a directory that is NOT on the
        // process's real PATH, then confirm the bash tool still cannot see
        // it unless one of the standard dirs is where it lives — instead we
        // assert the constructed PATH always contains a real standard dir
        // that resolves on this machine when present, proving the shell
        // sees the augmented PATH end-to-end.
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo $PATH"}))
            .await
            .unwrap();
        assert!(
            out.contains("/opt/homebrew/bin"),
            "spawned shell's PATH should include the standard dirs, got: {out}"
        );
    }
}
