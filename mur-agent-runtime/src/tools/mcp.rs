//! MCP-backed ToolExecutor: dispatches a single MCP tool via the pool.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use super::fs_policy::SessionCwd;
use super::{ToolError, ToolExecutor, ToolImage, ToolOutput};
use crate::llm::ToolDef;
use crate::mcp::pool::McpPool;
use crate::protocol::mcp_client::McpClient;

/// MCP tools that fan work out to other agents and therefore need to know
/// WHERE the work is. For these, a missing `cwd` is filled from the session
/// directory and flagged `cwd_inferred` so the receiving side can say it was
/// a guess (#1607). A fixed list, not a scan: injecting `cwd` into an
/// arbitrary third-party tool's arguments would be a schema violation.
pub const CWD_ROUTED_TOOLS: &[&str] = &["parallel_jobs"];

/// Fill in `cwd`/`cwd_inferred` for a routed tool when the model gave none.
/// An explicit `cwd` is never overridden — the model naming the target is
/// the fast path. Non-object inputs are left alone for the server to reject.
pub fn inject_session_cwd(tool: &str, input: &mut Value, session_cwd: Option<&SessionCwd>) {
    if !CWD_ROUTED_TOOLS.contains(&tool) {
        return;
    }
    let Some(cwd) = session_cwd else { return };
    let Some(obj) = input.as_object_mut() else {
        return;
    };
    if obj.get("cwd").is_some_and(|v| !v.is_null()) {
        return;
    }
    obj.insert(
        "cwd".into(),
        Value::String(cwd.current().display().to_string()),
    );
    obj.insert("cwd_inferred".into(), Value::Bool(true));
}

/// Default per-tool-call timeout when an MCP server entry sets no
/// `timeout_secs`. Deliberately short (spec 2026-09-12 execution-limits
/// §3.6): a tool that needs longer must return a handle and let the caller
/// poll — `fleet_run` and `parallel_jobs` do. Raising this is the wrong fix
/// for the next twenty-minute tool. Override per server via
/// `McpServerEntry.timeout_secs` when a server genuinely answers slowly.
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
    /// The agent's session directory, used to route fan-out tools
    /// (`CWD_ROUTED_TOOLS`) when the model names no target. `None` in
    /// contexts with no session (tests, headless probes).
    pub session_cwd: Option<SessionCwd>,
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    fn name(&self) -> &str {
        &self.wire_name
    }

    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn execute(&self, mut input: Value) -> Result<ToolOutput, ToolError> {
        inject_session_cwd(&self.tool, &mut input, self.session_cwd.as_ref());
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
            // and a failed call has nothing to show. A refusal is not a
            // failure to retry: the server said "may not".
            Err(if mur_common::authz::is_not_authorized(&text) {
                ToolError::NotAuthorized(text)
            } else {
                ToolError::Execution(text)
            })
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

    /// #1607: a routed tool with no `cwd` gets the session directory and is
    /// marked inferred; an explicit `cwd` is left exactly as the model gave it.
    #[test]
    fn injects_session_cwd_only_when_absent() {
        let session = SessionCwd::new(std::path::PathBuf::from("/home"));
        let _ = session.set(std::path::PathBuf::from("/proj"));

        let mut absent = json!({"jobs": [{"description": "x"}]});
        inject_session_cwd("parallel_jobs", &mut absent, Some(&session));
        assert_eq!(absent["cwd"], "/proj");
        assert_eq!(absent["cwd_inferred"], true);

        let mut explicit = json!({"jobs": [], "cwd": "/elsewhere"});
        inject_session_cwd("parallel_jobs", &mut explicit, Some(&session));
        assert_eq!(explicit["cwd"], "/elsewhere");
        assert!(explicit.get("cwd_inferred").is_none(), "{explicit}");
    }

    /// Only the allowlisted tools are touched: a third-party tool's arguments
    /// are not a place to smuggle fields into.
    #[test]
    fn leaves_unrouted_tools_and_missing_session_alone() {
        let session = SessionCwd::new(std::path::PathBuf::from("/home"));
        let mut other = json!({"q": "hi"});
        inject_session_cwd("mur_notes_search", &mut other, Some(&session));
        assert_eq!(other, json!({"q": "hi"}));

        let mut no_session = json!({"jobs": []});
        inject_session_cwd("parallel_jobs", &mut no_session, None);
        assert_eq!(no_session, json!({"jobs": []}));
    }
}
