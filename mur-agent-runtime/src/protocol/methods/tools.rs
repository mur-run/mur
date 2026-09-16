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

pub struct ToolsCallHandler {
    runner: Arc<TaskRunner>,
}

impl ToolsCallHandler {
    pub fn new(r: Arc<TaskRunner>) -> Self {
        Self { runner: r }
    }
}

#[async_trait]
impl MethodHandler for ToolsCallHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let task_id = p
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::InvalidParams("missing task_id".into()))?
            .to_string();
        let name = p
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::InvalidParams("missing name".into()))?
            .to_string();
        let call_id = p
            .get("call_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let input = p.get("arguments").cloned().unwrap_or(Value::Null);

        let call = crate::llm::ToolCallResult {
            call_id: call_id.clone(),
            tool_name: name,
            input,
        };

        // The same pair an in-process turn runs, in the same order: gate
        // first, then execute with whatever the gate decided. Calling
        // `handle_tool_call` without `gate_response` would skip the HITL
        // prompt for an `Ask` tool and execute it unasked.
        let guarded = self.runner.guarded();
        let (decisions, step_ids) = guarded
            .gate_response(&task_id, std::slice::from_ref(&call))
            .await;
        let entry = match guarded
            .handle_tool_call(
                &task_id,
                &call,
                decisions.get(&call_id).cloned(),
                step_ids.get(&call_id).cloned(),
            )
            .await
        {
            Ok(e) => e,
            // A refusal is an answer, not a transport failure. `handle_tool_call`
            // reports a HITL denial as `Err(TaskError { code: "hitl_denied" })`,
            // and MCP's convention is that a tool which refused returns a
            // result carrying `isError` so the model can read it — a protocol
            // error would tell the CLI the *call* broke and tell the model
            // nothing. The code travels with it so the shim can still tell a
            // refusal from a crash instead of guessing from a message string.
            Err(e) => {
                return Ok(json!({
                    "call_id": call_id,
                    "content": e.message,
                    "is_error": true,
                    "error_code": e.code,
                }));
            }
        };

        Ok(json!({
            "call_id": entry.call_id,
            "content": entry.content,
            "is_error": entry.is_error,
            "status": entry.status,
        }))
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

    #[tokio::test]
    async fn tools_call_requires_a_task_id() {
        // Not pedantry: task_id is what gives a spawned bash job an owner and
        // what routes an approval prompt. Defaulting it would detach both
        // silently, which is why this is an error rather than a fallback.
        let h = ToolsCallHandler::new(Arc::new(TaskRunner::new_stub_echo()));
        let err = h
            .handle(Some(json!({ "name": "read_file" })), &ctx())
            .await
            .expect_err("must reject");
        assert!(matches!(err, HandlerError::InvalidParams(_)), "{err:?}");
    }

    #[tokio::test]
    async fn an_unknown_tool_is_reported_not_executed() {
        let h = ToolsCallHandler::new(Arc::new(TaskRunner::new_stub_echo()));
        let out = h
            .handle(
                Some(json!({ "task_id": "t-1", "name": "no_such_tool" })),
                &ctx(),
            )
            .await
            .expect("handler");
        assert_eq!(out["is_error"], json!(true));
        assert!(
            out["content"]
                .as_str()
                .unwrap_or_default()
                .contains("unknown tool"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn the_call_id_is_echoed_so_the_shim_can_correlate() {
        let h = ToolsCallHandler::new(Arc::new(TaskRunner::new_stub_echo()));
        let out = h
            .handle(
                Some(json!({
                    "task_id": "t-1",
                    "name": "no_such_tool",
                    "call_id": "given-by-caller",
                })),
                &ctx(),
            )
            .await
            .expect("handler");
        assert_eq!(out["call_id"], json!("given-by-caller"));
    }
}
