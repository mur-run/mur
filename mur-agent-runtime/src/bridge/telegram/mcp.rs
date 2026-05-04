//! Telegram-specific MCP tool surface (M-c2.5).
//!
//! When the bridge runs in MCP mode it exposes a stdio JSON-RPC server with
//! a single tool: `chat.send_message { chat_id: i64, body: String }`.
//! The user-agent invokes this via the bridge's MCP entry registered in
//! `profile.mcp_servers[]`. The Throttle adapter (already wired in M-c2.2's
//! Bot construction) handles per-chat / global rate-limit automatically.
//!
//! ## JSON-RPC surface (subset of MCP `2024-11-05`)
//!
//! - `tools/list` — returns `[{name, description, inputSchema}]`
//! - `tools/call { name, arguments }` — dispatches to the named tool
//!
//! ## Stdio loop
//!
//! [`serve_mcp`] reads newline-delimited JSON-RPC requests from any
//! `AsyncBufRead`, dispatches each via [`handle_jsonrpc`], and writes the
//! response back as a newline-delimited frame. Tests can pipe in-memory
//! cursors; production wires `tokio::io::stdin()` / `stdout()`.
//!
//! ## Test seam
//!
//! [`McpDeps::bot`] is currently `Arc<MockBot>` — that's the seam M-c2.2
//! built. When the real `Throttle<CacheMe<Bot>>` wiring lands (Phase C2.6
//! polish), this becomes a generic `Arc<dyn TgBotLike + Send + Sync>` or
//! similar; the JSON-RPC dispatcher itself is unaffected.

use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::bridge::telegram::mock::MockBot;

/// Dependencies the JSON-RPC dispatcher closes over. Currently just the
/// bot handle; future fields (per-chat ACL, rate-limit observer) slot in
/// here without touching call sites.
pub struct McpDeps {
    pub bot: Arc<MockBot>,
}

/// Dispatch one JSON-RPC request. Returns the JSON-RPC response value
/// (always shaped `{jsonrpc, id, result|error}`); errors in tool payloads
/// surface as `Err(anyhow::Error)` so the caller can decide whether to
/// reply with a JSON-RPC error frame or close the connection.
pub async fn handle_jsonrpc(req: Value, deps: &McpDeps) -> anyhow::Result<Value> {
    let method = req["method"].as_str().unwrap_or("");
    let id = req["id"].clone();
    match method {
        "tools/list" => Ok(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [{
                    "name": "chat.send_message",
                    "description": "Send a Telegram message to a chat.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "chat_id": {"type": "integer"},
                            "body": {"type": "string"}
                        },
                        "required": ["chat_id", "body"]
                    }
                }]
            }
        })),
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            if name != "chat.send_message" {
                anyhow::bail!("unknown tool {}", name);
            }
            let args = &req["params"]["arguments"];
            let chat_id = args["chat_id"].as_i64().unwrap_or(0);
            let body = args["body"].as_str().unwrap_or("").to_string();
            // MockBot stand-in for M-c2.5: append to `sent_messages`. When
            // the real teloxide bot lands we'll call
            // `bot.send_message(ChatId(chat_id), body).await?`; the
            // Throttle<CacheMe<Bot>> wrapper from M-c2.2 enforces the
            // 30/sec global cap.
            deps.bot.sent_messages.lock().unwrap().push((chat_id, body));
            Ok(json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}}))
        }
        _ => anyhow::bail!("unknown method {}", method),
    }
}

/// Newline-delimited JSON-RPC server loop. Reads one request per line,
/// dispatches via [`handle_jsonrpc`], writes the response back as a
/// newline-terminated JSON frame. Returns when stdin reaches EOF or on
/// the first parse error.
///
/// This signature accepts any `AsyncBufRead + AsyncWrite` so tests can
/// pipe `tokio::io::BufReader::new(Cursor::new(...))` /
/// `tokio::io::sink()` etc. without spawning a real process.
pub async fn serve_mcp<R, W>(mut reader: R, mut writer: W, deps: &McpDeps) -> anyhow::Result<()>
where
    R: AsyncBufReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(()); // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = serde_json::from_str(trimmed)?;
        let resp = handle_jsonrpc(req, deps).await?;
        let mut bytes = serde_json::to_vec(&resp)?;
        bytes.push(b'\n');
        writer.write_all(&bytes).await?;
        writer.flush().await?;
    }
}
