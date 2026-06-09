//! MCP-backed ToolExecutor: dispatches a single MCP tool via the pool.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use super::{ToolError, ToolExecutor};
use crate::llm::ToolDef;
use crate::mcp::pool::McpPool;
use crate::protocol::mcp_client::McpClient;

/// Default per-tool-call timeout when an MCP server entry sets no
/// `timeout_secs`. Override per server via `McpServerEntry.timeout_secs`.
pub const DEFAULT_MCP_TOOL_TIMEOUT_SECS: u64 = 120;
pub const MCP_TOOL_TIMEOUT: Duration = Duration::from_secs(DEFAULT_MCP_TOOL_TIMEOUT_SECS);

/// Convert an MCP `tools/call` result to a display string.
pub fn render_mcp_result(result: &Value) -> String {
    let Some(content) = result.get("content").and_then(|c| c.as_array()) else {
        return serde_json::to_string(result).unwrap_or_default();
    };
    if content.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for block in content {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    out.push_str(t);
                    out.push('\n');
                }
            }
            Some("image") => out.push_str("[image]\n"),
            Some("resource") => {
                let uri = block
                    .get("resource")
                    .and_then(|r| r.get("uri"))
                    .and_then(|u| u.as_str())
                    .unwrap_or("?");
                out.push_str(&format!("[resource: {uri}]\n"));
            }
            _ => {}
        }
    }
    out.trim_end().to_string()
}

/// A [`ToolExecutor`] backed by a single MCP tool, dispatched via the shared pool.
pub struct McpToolExecutor {
    pub wire_name: String,
    pub server: String,
    pub tool: String,
    pub def: ToolDef,
    pub pool: Arc<McpPool>,
    pub timeout: Duration,
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    fn name(&self) -> &str {
        &self.wire_name
    }

    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn execute(&self, input: Value) -> Result<String, ToolError> {
        let client_arc: Arc<Mutex<McpClient>> = self
            .pool
            .client(&self.server)
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))?;

        let result = tokio::time::timeout(self.timeout, async {
            client_arc
                .lock()
                .await
                .call_tool(&self.tool, input)
                .await
                .map_err(|e| ToolError::Execution(e.to_string()))
        })
        .await
        .map_err(|_| {
            ToolError::Execution(format!(
                "tool `{}` timed out after {:?}",
                self.wire_name, self.timeout
            ))
        })??;

        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let text = render_mcp_result(&result);
        if is_error {
            Err(ToolError::Execution(text))
        } else {
            Ok(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_text_block() {
        let r = json!({"content": [{"type": "text", "text": "hello world"}]});
        assert_eq!(render_mcp_result(&r), "hello world");
    }

    #[test]
    fn render_image_block() {
        let r = json!({"content": [{"type": "image"}]});
        assert_eq!(render_mcp_result(&r), "[image]");
    }

    #[test]
    fn render_multi_block() {
        let r = json!({"content": [
            {"type": "text", "text": "line1"},
            {"type": "image"}
        ]});
        let out = render_mcp_result(&r);
        assert!(out.contains("line1"));
        assert!(out.contains("[image]"));
    }

    #[test]
    fn render_fallback_json() {
        let r = json!({"notContent": "foo"});
        assert!(!render_mcp_result(&r).is_empty());
    }
}
