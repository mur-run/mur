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
    let deps = McpDeps {
        bot: bot.clone(),
        allowed_chats: vec![100],
    };
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
    let deps = McpDeps {
        bot: bot.clone(),
        allowed_chats: vec![100],
    };
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
    let deps = McpDeps {
        bot: bot.clone(),
        allowed_chats: vec![100],
    };
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

#[tokio::test]
async fn chat_send_message_rejects_chat_not_in_allowlist() {
    // Regression: the agent must not be able to message an arbitrary chat.
    let bot = Arc::new(MockBot::default());
    let deps = McpDeps {
        bot: bot.clone(),
        allowed_chats: vec![100],
    };
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"chat.send_message",
            "arguments":{"chat_id":999,"body":"to a stranger"}
        }
    });
    assert!(
        handle_jsonrpc(req, &deps).await.is_err(),
        "send to non-allowlisted chat must be rejected"
    );
    assert!(
        bot.sent_messages.lock().unwrap().is_empty(),
        "nothing should reach the bot"
    );
}

// ─────────────────────────────────────────────────────────────────────
// M-c2.6 — rate-limit verification
// ─────────────────────────────────────────────────────────────────────

/// M-c2.6.1 — token-bucket caps the global send-rate to 30/s.
///
/// We burst 50 `chat.send_message` requests. By the 1 s mark only 30
/// must have been delivered to the wire; the rest queue behind the
/// throttle. We also assert the burst takes ≥0.9 s end-to-end, proving
/// the throttle is actually serialising and not just pre-trimming.
#[tokio::test]
async fn throttle_caps_global_to_thirty_per_second() {
    let bot = Arc::new(MockBot::throttled(30));
    let deps = McpDeps {
        bot: bot.clone(),
        allowed_chats: (1..=50).collect(),
    };
    let start = std::time::Instant::now();
    for i in 1..=50 {
        let req = serde_json::json!({
            "jsonrpc":"2.0","id":i,"method":"tools/call","params":{
                "name":"chat.send_message","arguments":{"chat_id":i,"body":"x"}
            }
        });
        let _ = handle_jsonrpc(req, &deps).await;
    }
    let dur = start.elapsed();
    let after_one_second = bot.delivered_within(std::time::Duration::from_secs(1));
    assert!(
        after_one_second <= 30,
        "delivered {after_one_second} in 1s, cap is 30"
    );
    assert!(
        dur >= std::time::Duration::from_millis(900),
        "burst should serialize over time, was {dur:?}"
    );
    let total = bot.sent_messages.lock().unwrap().len();
    assert_eq!(total, 50, "all 50 messages must eventually reach the wire");
}

/// M-c2.6.2 — per-chat pacing enforces 1 message/second to a single chat.
///
/// 5 messages to the same `chat_id` should take ≥4 s (4 × 1 s gaps).
#[tokio::test]
async fn per_chat_paces_at_one_per_second() {
    let bot = Arc::new(MockBot::per_chat(1));
    let deps = McpDeps {
        bot: bot.clone(),
        allowed_chats: vec![42],
    };
    let start = std::time::Instant::now();
    for i in 0..5 {
        let req = serde_json::json!({
            "jsonrpc":"2.0","id":i,"method":"tools/call","params":{
                "name":"chat.send_message","arguments":{"chat_id":42,"body":format!("m{i}")}
            }
        });
        let _ = handle_jsonrpc(req, &deps).await;
    }
    let dur = start.elapsed();
    assert!(
        dur >= std::time::Duration::from_secs(4),
        "5 msgs to one chat at 1/s should take >=4s, was {dur:?}"
    );
}

/// Per-chat policy must NOT block other chats — each chat_id keys its
/// own pacing window.
#[tokio::test]
async fn per_chat_does_not_block_other_chats() {
    let bot = Arc::new(MockBot::per_chat(1));
    let deps = McpDeps {
        bot: bot.clone(),
        allowed_chats: (1000..1005).collect(),
    };
    let start = std::time::Instant::now();
    // First message to each of 5 distinct chats — none have been sent
    // to before, so all should fire immediately.
    for chat_id in 1000..1005 {
        let req = serde_json::json!({
            "jsonrpc":"2.0","id":chat_id,"method":"tools/call","params":{
                "name":"chat.send_message","arguments":{"chat_id":chat_id,"body":"first"}
            }
        });
        let _ = handle_jsonrpc(req, &deps).await;
    }
    let dur = start.elapsed();
    assert!(
        dur < std::time::Duration::from_millis(500),
        "5 first-sends to distinct chats should be near-instant, was {dur:?}"
    );
    assert_eq!(bot.sent_messages.lock().unwrap().len(), 5);
}
