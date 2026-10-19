//! #1907 follow-up: an `egress/denied` raised while a turn's tool call is
//! using an MCP server names that turn, so the runtime can route it to that
//! turn's connection instead of every connection on the agent's socket.

use std::time::Duration;

use mur_agent_runtime::llm::ToolDef;
use mur_agent_runtime::mcp::pool::McpPool;
use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_agent_runtime::sandbox::egress_proxy::start_egress_proxy;
use mur_agent_runtime::tools::ToolExecutor;
use mur_agent_runtime::tools::bash_jobs::CURRENT_TASK_ID;
use mur_agent_runtime::tools::mcp::McpToolExecutor;
use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};
use mur_common::egress_event::{DENIED_METHOD, EgressDenied};

#[tokio::test]
async fn a_denied_fetch_inside_a_turns_tool_call_names_that_turn() {
    let proxy = start_egress_proxy("mur").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
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
    let exec = McpToolExecutor {
        wire_name: "mcp__browser__connect".into(),
        server: "browser".into(),
        tool: "connect".into(),
        def: ToolDef {
            name: "mcp__browser__connect".into(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        pool: pool.clone(),
        timeout: Duration::from_secs(10),
        session_cwd: None,
    };

    let out = CURRENT_TASK_ID
        .scope(
            "task-a".to_string(),
            exec.execute(serde_json::json!({"host": "blocked.example"})),
        )
        .await
        .expect("tool call");
    assert!(out.text.contains("403"), "proxy must refuse: {}", out.text);

    let n = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a denial must be emitted")
        .unwrap();
    assert_eq!(n["method"], DENIED_METHOD);
    let ev: EgressDenied = serde_json::from_value(n["params"].clone()).unwrap();
    assert_eq!(ev.task_id.as_deref(), Some("task-a"));
    pool.shutdown().await;
}
