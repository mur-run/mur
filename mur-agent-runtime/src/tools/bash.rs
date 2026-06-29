use std::path::PathBuf;
use tokio::process::Command;

use super::{ToolError, ToolExecutor};
use crate::llm::ToolDef;

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

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            Command::new("bash")
                .arg("-c")
                .arg(&command)
                .current_dir(&working_dir)
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
}
