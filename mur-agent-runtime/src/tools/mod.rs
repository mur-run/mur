pub mod bash;
pub mod edit_file;
pub(crate) mod fs_policy;
pub mod mcp;
pub mod naming;
pub mod read_file;
pub mod registry;
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

#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn def(&self) -> ToolDef;
    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError>;
}
