//! Track C2 — Telegram bridge outbound MCP server tests.
//!
//! Covers M-c2.5.1 .. M-c2.5.4: the bridge exposes a stdio MCP server with
//! one tool (`chat.send_message`). Tests drive the JSON-RPC dispatcher
//! directly with `serde_json::Value` requests; the inner [`MockBot`]
//! collects sent `(chat_id, body)` pairs so we can assert side effects.

use mur_agent_runtime::bridge::telegram::mcp::{McpDeps, handle_jsonrpc};
use mur_agent_runtime::bridge::telegram::mock::MockBot;
use std::sync::Arc;

#[tokio::test]
async fn list_tools_returns_chat_send_message() {
    let bot = Arc::new(MockBot::default());
    let deps = McpDeps { bot: bot.clone() };
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"tools/list","params":{}
    });
    let resp = handle_jsonrpc(req, &deps).await.unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "chat.send_message"));
}

#[tokio::test]
async fn chat_send_message_pushes_to_bot() {
    let bot = Arc::new(MockBot::default());
    let deps = McpDeps { bot: bot.clone() };
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"chat.send_message",
            "arguments":{"chat_id":100,"body":"hi"}
        }
    });
    let resp = handle_jsonrpc(req, &deps).await.unwrap();
    assert_eq!(resp["result"]["ok"], true);
    let sent = bot.sent_messages.lock().unwrap().clone();
    assert_eq!(sent, vec![(100i64, "hi".to_string())]);
}

#[tokio::test]
async fn mcp_stdio_loop_handles_two_calls() {
    let bot = Arc::new(MockBot::default());
    let deps = McpDeps { bot: bot.clone() };
    for i in 0..2 {
        let req = serde_json::json!({
            "jsonrpc":"2.0","id":i,"method":"tools/call","params":{
                "name":"chat.send_message",
                "arguments":{"chat_id":100,"body":format!("m{i}")}
            }
        });
        let _ = handle_jsonrpc(req, &deps).await.unwrap();
    }
    let sent = bot.sent_messages.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
}
