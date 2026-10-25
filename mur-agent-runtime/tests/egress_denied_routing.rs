//! #1907 follow-up: an `egress/denied` raised while a turn's tool call is
//! using an MCP server names that turn, so the runtime can route it to that
//! turn's connection instead of every connection on the agent's socket.

use std::sync::Arc;
use std::time::Duration;

use mur_agent_runtime::llm::ToolDef;
use mur_agent_runtime::mcp::pool::McpPool;
use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_agent_runtime::sandbox::egress_proxy::start_egress_proxy;
use mur_agent_runtime::tools::bash_jobs::CURRENT_TASK_ID;
use mur_agent_runtime::tools::mcp::McpToolExecutor;
use mur_agent_runtime::tools::{ToolError, ToolExecutor, ToolOutput};
use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};
use mur_common::egress_event::{DENIED_METHOD, EgressDenied};
use tokio::sync::mpsc::Receiver;

/// A pool with one proxied `mock_mcp` server named `browser`, and the
/// receiving end of the proxy's event sink.
async fn proxied_pool() -> (Arc<McpPool>, Receiver<serde_json::Value>) {
    let proxy = start_egress_proxy("mur").await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    proxy.attach_events(tx);
    let entry = McpServerEntry {
        name: "browser".into(),
        command: env!("CARGO_BIN_EXE_mock_mcp").into(),
        network: Some(McpServerNetwork {
            mode: McpNetMode::Restricted,
            allow_hosts: vec!["allowed.example".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let pool = McpPool::new(vec![entry], SandboxPolicy::default(), Some(proxy));
    (pool, rx)
}

fn tool(pool: &Arc<McpPool>, tool: &str) -> McpToolExecutor {
    let wire = format!("mcp__browser__{tool}");
    McpToolExecutor {
        wire_name: wire.clone(),
        server: "browser".into(),
        tool: tool.into(),
        def: ToolDef {
            name: wire,
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        pool: pool.clone(),
        timeout: Duration::from_secs(10),
        session_cwd: None,
        serena_project: None,
    }
}

async fn call_as(
    task: &str,
    exec: &McpToolExecutor,
    input: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    CURRENT_TASK_ID
        .scope(task.to_string(), exec.execute(input))
        .await
}

/// `task` fetches a blocked host; returns the denial the proxy emitted.
async fn denied_fetch_as(
    task: &str,
    pool: &Arc<McpPool>,
    rx: &mut Receiver<serde_json::Value>,
    host: &str,
) -> EgressDenied {
    let out = call_as(
        task,
        &tool(pool, "connect"),
        serde_json::json!({"host": host}),
    )
    .await
    .expect("tool call");
    assert!(out.text.contains("403"), "proxy must refuse: {}", out.text);
    let n = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a denial must be emitted")
        .unwrap();
    assert_eq!(n["method"], DENIED_METHOD);
    serde_json::from_value(n["params"].clone()).unwrap()
}

#[tokio::test]
async fn a_denied_fetch_inside_a_turns_tool_call_names_that_turn() {
    let (pool, mut rx) = proxied_pool().await;
    let ev = denied_fetch_as("task-a", &pool, &mut rx, "blocked.example").await;
    assert_eq!(ev.task_id.as_deref(), Some("task-a"));
    pool.shutdown().await;
}

/// A JSON-RPC error ends the call but does not prove the tool never ran: a
/// browser tool can start a navigation and then fail internally. When the
/// next turn's call then sees a denial, it may be A's page still loading, so
/// it must broadcast rather than land on task-b alone.
#[tokio::test]
async fn a_jsonrpc_error_keeps_the_turn_a_suspect_for_trailing_traffic() {
    let (pool, mut rx) = proxied_pool().await;
    let err = call_as("task-a", &tool(&pool, "rpc_error"), serde_json::json!({}))
        .await
        .expect_err("the server failed");
    assert!(err.to_string().contains("refused"), "{err}");
    let ev = denied_fetch_as("task-b", &pool, &mut rx, "blocked.example").await;
    assert_eq!(ev.task_id, None, "task-a's page may be the source");
    pool.shutdown().await;
}

/// The request reached the server but the client failed to read its reply
/// (here: an undecodable line). That is a local failure, not an answer — the
/// server may still be running the call, so a later denial must not be
/// pinned on whichever turn uses the server next.
#[tokio::test]
async fn a_reply_the_client_could_not_read_leaves_later_denials_unattributed() {
    let (pool, mut rx) = proxied_pool().await;
    let err = call_as("task-a", &tool(&pool, "garble"), serde_json::json!({}))
        .await
        .expect_err("undecodable reply");
    assert!(err.to_string().contains("json"), "{err}");
    let ev = denied_fetch_as("task-b", &pool, &mut rx, "blocked.example").await;
    assert_eq!(ev.task_id, None);
    pool.shutdown().await;
}
