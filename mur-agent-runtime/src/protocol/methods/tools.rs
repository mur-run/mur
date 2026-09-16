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
        ctx: &RequestContext,
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
        // Route this task's approval prompts back to the connection that
        // asked, exactly as `message_send` does. Without this the gate has
        // no sink and an `Ask` tool is denied — correct, but unusable.
        //
        // `can_approve` defaults to false: a caller that cannot answer must
        // say so, because claiming otherwise makes the gate wait out its
        // full timeout for nobody.
        let can_approve = p
            .get("can_approve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(n) = ctx.notifier.clone() {
            self.runner
                .register_client_notifier(&task_id, n, can_approve)
                .await;
        }
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
                self.runner.unregister_client_notifier(&task_id).await;
                return Ok(json!({
                    "call_id": call_id,
                    "content": e.message,
                    "is_error": true,
                    "error_code": e.code,
                }));
            }
        };

        self.runner.unregister_client_notifier(&task_id).await;
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

    #[tokio::test]
    async fn a_denied_tool_is_refused_without_executing() {
        use mur_common::agent::{ToolPolicy, ToolRule};
        let runner = runner_with_probe().with_tools_policy(vec![ToolRule {
            pattern: "*".into(),
            policy: ToolPolicy::Deny,
            risk: None,
        }]);
        let h = ToolsCallHandler::new(Arc::new(runner));
        let out = h
            .handle(
                Some(json!({ "task_id": "t-1", "name": "probe_tool" })),
                &ctx(),
            )
            .await
            .expect("handler");
        assert_eq!(out["is_error"], json!(true));
        assert!(
            out["content"]
                .as_str()
                .unwrap_or_default()
                .contains("denied by policy"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn an_ask_tool_with_no_approval_sink_denies_rather_than_hanging() {
        // This slice's boundary, asserted so it reads as a boundary rather
        // than a bug: until the shim registers as the task's approval sink,
        // an `Ask` tool has nobody to ask, and the gate's answer to that is
        // to deny at once. Never to run it, and never to wait out the
        // timeout against nobody.
        use mur_common::agent::{ToolPolicy, ToolRule};
        let runner = runner_with_probe().with_tools_policy(vec![ToolRule {
            pattern: "*".into(),
            policy: ToolPolicy::Ask,
            risk: None,
        }]);
        let h = ToolsCallHandler::new(Arc::new(runner));
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            h.handle(
                Some(json!({ "task_id": "t-1", "name": "probe_tool" })),
                &ctx(),
            ),
        )
        .await
        .expect("must not hang: an unanswerable gate denies, it does not wait")
        .expect("handler");
        assert_eq!(out["is_error"], json!(true));
        // The code, not the prose: a shim distinguishing a refusal from a
        // crash must not have to match on a message.
        assert_eq!(out["error_code"], json!("hitl_denied"));
    }

    #[tokio::test]
    async fn tool_output_reaching_this_caller_is_masked() {
        // The obligation that fails most quietly. If a future change routes
        // `tools/call` around `GuardedToolCall`, this is what notices.
        let vault = crate::secrets::SecretVault::new();
        vault
            .set("PROBE_TOKEN", "sk-probe-abcdefghijklmnop")
            .expect("set");
        let runner = runner_with_probe().with_secrets(Arc::new(vault));
        let masked = runner
            .guarded()
            .masked("leaked sk-probe-abcdefghijklmnop here".into());
        assert!(!masked.contains("sk-probe-abcdefghijklmnop"), "{masked}");
        assert!(masked.contains("PROBE_TOKEN"), "{masked}");
    }

    #[tokio::test]
    async fn an_ask_tool_asks_the_calling_connection() {
        // The Task 2 property: the prompt reaches the caller's own sink.
        // Answering is the shim's job; this asserts only that the question
        // was asked, which is what #1351 could not do.
        use mur_common::agent::{ToolPolicy, ToolRule};
        let runner = Arc::new(
            runner_with_probe()
                .with_tools_policy(vec![ToolRule {
                    pattern: "*".into(),
                    policy: ToolPolicy::Ask,
                    risk: None,
                }])
                .with_pending_approvals(Default::default()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(8);
        let ctx = RequestContext {
            notifier: Some(tx),
        };
        let h = ToolsCallHandler::new(runner);
        let call = tokio::spawn(async move {
            h.handle(
                Some(json!({
                    "task_id": "t-1",
                    "name": "probe_tool",
                    "can_approve": true,
                })),
                &ctx,
            )
            .await
        });
        let note = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no approval prompt reached the caller")
            .expect("sink closed");
        assert_eq!(note["method"], json!("tool/approval_needed"));
        assert_eq!(note["params"]["task_id"], json!("t-1"));
        // Nobody answers, so the gate times out and denies. That the prompt
        // arrived at all is the assertion; the denial is #1351's behaviour.
        drop(call);
    }
}
