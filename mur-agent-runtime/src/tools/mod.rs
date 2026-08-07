pub mod bash;
pub mod edit_file;
pub mod fleet_run;
pub(crate) mod fs_policy;
pub mod mcp;
pub mod naming;
pub mod open_item;
pub mod read_file;
pub mod registry;
pub mod remember;
pub(crate) mod suggest;
pub mod write_file;

use crate::llm::ToolDef;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool execution failed: {0}")]
    Execution(String),
    #[error("unknown tool: {0}")]
    Unknown(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// Structural status of a tool execution.
///
/// This exists because the bash tool used to encode exit code and sandbox
/// denial as text markers (e.g. `"[exit code: N]"`, `"[sandbox] ..."`) glued
/// onto the end of its plain-`String` output. Downstream code then grepped
/// those markers back out of the string. That let file *content* forge
/// execution status: reading a file that merely happened to contain the
/// marker text made a successful read render as a sandbox denial. Status now
/// travels as data, structurally, alongside the text - never inside it.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ToolStatus {
    #[default]
    Ok,
    Failed {
        exit_code: i32,
    },
    Denied {
        detail: String,
    },
}

/// Result of a tool execution: the model-facing text plus the real,
/// structural execution status (see [`ToolStatus`]).
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub text: String,
    pub status: ToolStatus,
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        ToolOutput {
            text,
            status: ToolStatus::Ok,
        }
    }
}

#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn def(&self) -> ToolDef;
    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError>;
}
