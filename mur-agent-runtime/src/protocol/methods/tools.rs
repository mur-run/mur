//! `tools/*` handlers — the agent's tools, over its own socket.
//!
//! These exist so the MCP shim has something to forward to. The shim speaks
//! MCP on stdio to a spawned CLI and nothing else: it holds no tools, no
//! policy and no vault, so every obligation stays on this side of the
//! socket. See `docs/superpowers/specs/2026-09-16-mur-tool-mcp-server-design.md`.
//!
//! `tools/call` runs the same `gate_response` → `handle_tool_call` pair an
//! in-process turn runs. Not a similar pair — the same one, so a change to
//! the gate cannot apply to one caller and miss the other.

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::TaskRunner;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct ToolsListHandler {
    runner: Arc<TaskRunner>,
}

impl ToolsListHandler {
    pub fn new(r: Arc<TaskRunner>) -> Self {
        Self { runner: r }
    }
}

#[async_trait]
impl MethodHandler for ToolsListHandler {
    async fn handle(
        &self,
        _params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let tools: Vec<Value> = self
            .runner
            .guarded()
            .tools
            .iter()
            .map(|t| {
                let d = t.def();
                json!({
                    "name": d.name,
                    "description": d.description,
                    "inputSchema": d.input_schema,
                })
            })
            .collect();
        Ok(json!({ "tools": tools }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RequestContext {
        // No per-connection notifier: these tests drive the handler directly,
        // not through a socket.
        RequestContext::none()
    }

    /// A tool that records nothing and returns a fixed string.
    ///
    /// `TaskRunner::new_stub_echo()` registers **no tools at all**, so every
    /// test here must supply its own. Without this, `tools/list` would assert
    /// `0 == 0` and pass while proving nothing.
    struct ProbeTool;

    #[async_trait::async_trait]
    impl crate::tools::ToolExecutor for ProbeTool {
        fn name(&self) -> &str {
            "probe_tool"
        }
        fn def(&self) -> crate::llm::ToolDef {
            crate::llm::ToolDef {
                name: "probe_tool".into(),
                description: "a tool for tests".into(),
                input_schema: json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _input: Value,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            Ok("probe ran".to_string().into())
        }
    }

    fn runner_with_probe() -> TaskRunner {
        TaskRunner::new_stub_echo().with_tools(vec![Arc::new(ProbeTool)])
    }

    #[tokio::test]
    async fn tools_list_reports_every_tool_in_mcp_shape() {
        let runner = Arc::new(runner_with_probe());
        let out = ToolsListHandler::new(runner.clone())
            .handle(None, &ctx())
            .await
            .expect("handler");
        let listed = out["tools"].as_array().expect("tools array");
        assert_eq!(listed.len(), 1, "expected exactly the probe tool");
        assert_eq!(listed[0]["name"], json!("probe_tool"));
        for t in listed {
            // The shim forwards these verbatim, so the MCP spelling is the
            // one that has to be right here rather than one hop later.
            assert!(t["name"].is_string(), "name missing: {t}");
            assert!(t["description"].is_string(), "description missing: {t}");
            assert!(t["inputSchema"].is_object(), "inputSchema missing: {t}");
        }
    }
}
