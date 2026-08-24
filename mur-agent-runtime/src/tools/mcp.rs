//! MCP-backed ToolExecutor: dispatches a single MCP tool via the pool.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use super::{ToolError, ToolExecutor, ToolImage, ToolOutput};
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

/// Pull the image blocks out of an MCP `tools/call` result.
///
/// MCP image content is `{"type":"image","data":<base64>,"mimeType":…}`;
/// the Messages API wants `source.media_type`, so the rename happens here and
/// nowhere else. [`render_mcp_result`] still writes an `[image]` placeholder
/// into the text for the same block — that is deliberate, and it is what an
/// adapter without vision tool results shows the model.
///
/// Unsupported media types and oversize images are dropped
/// ([`ToolImage::is_supported`]): a provider rejects the whole turn over one
/// bad image, so losing the picture beats losing the turn.
pub fn extract_mcp_images(result: &Value) -> Vec<ToolImage> {
    let Some(content) = result.get("content").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("image"))
        .filter_map(|b| {
            Some(ToolImage {
                media_type: b.get("mimeType").and_then(|m| m.as_str())?.to_string(),
                data: b.get("data").and_then(|d| d.as_str())?.to_string(),
            })
        })
        .filter(ToolImage::is_supported)
        .collect()
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

    async fn execute(&self, input: Value) -> Result<ToolOutput, ToolError> {
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
            // A timeout says we stopped waiting — it does NOT say the call
            // failed. MCP has no cancel: the server keeps running the tool.
            // Reporting this as a plain failure taught agents to "recover" by
            // re-dispatching work that was already in flight, or to burn a
            // turn on `sleep` waiting for a result they'd been told was dead.
            ToolError::Execution(format!(
                "tool `{}` did not return within {:?}; MUR stopped waiting, but the server may \
                 still be running it — treat the outcome as unknown, check for side effects \
                 before retrying. Raise `timeout_secs` for MCP server `{}` if this tool is \
                 expected to run long.",
                self.wire_name, self.timeout, self.server
            ))
        })??;

        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let text = render_mcp_result(&result);
        if is_error {
            // An error result carries no images: the error path is a String,
            // and a failed call has nothing to show.
            Err(ToolError::Execution(text))
        } else {
            let mut out: ToolOutput = text.into();
            out.images = extract_mcp_images(&result);
            Ok(out)
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

    /// MCP names the field `mimeType`; the Messages API wants `media_type`.
    /// The rename happens in `extract_mcp_images` and nowhere else, so this
    /// pins it — and pins that junk media types are dropped rather than sent
    /// (a provider rejects the entire turn over one bad image).
    #[test]
    fn extracts_mcp_images_and_drops_unusable_ones() {
        let r = json!({"content": [
            {"type": "text", "text": "here"},
            {"type": "image", "data": "QUJD", "mimeType": "image/png"},
            {"type": "image", "data": "QUJD", "mimeType": "image/tiff"},
            {"type": "image", "data": "QUJD"}
        ]});
        let imgs = extract_mcp_images(&r);
        assert_eq!(imgs.len(), 1, "only the supported, well-formed image");
        assert_eq!(imgs[0].media_type, "image/png");
        assert_eq!(imgs[0].data, "QUJD");

        // Negative control: a result with no image block yields nothing,
        // so the filter above is real and not "always returns one".
        let text_only = json!({"content": [{"type": "text", "text": "hi"}]});
        assert!(extract_mcp_images(&text_only).is_empty());
    }
}
