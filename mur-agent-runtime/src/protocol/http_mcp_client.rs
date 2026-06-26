//! Streamable HTTP MCP transport.
//!
//! POSTs JSON-RPC 2.0 to a single endpoint and parses JSON-or-SSE responses,
//! per the MCP Streamable HTTP spec.  Bearer auth and `Mcp-Session-Id` session
//! tracking are both supported.
//!
// ponytail: reuse stdio client's existing JSON→type code — don't write second parser.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};

use super::mcp_client::{InitializeInfo, McpError, ToolInfo};
use super::mcp_sse::{jsonrpc_result_for, parse_sse_events};

const PROTOCOL_VERSION: &str = "2025-06-18";

pub struct HttpMcpClient {
    http: reqwest::Client,
    url: String,
    bearer: Option<String>,
    session_id: Mutex<Option<String>>,
    next_id: AtomicI64,
}

/// Build a JSON-RPC 2.0 request object.
pub fn jsonrpc_request(id: i64, method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

impl HttpMcpClient {
    /// Create a client pointed at `url`.  `bearer` is attached as
    /// `Authorization: Bearer <token>` on every request when present.
    pub async fn connect(url: &str, bearer: Option<String>) -> Result<Self, McpError> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(Self {
            http,
            url: url.to_string(),
            bearer,
            session_id: Mutex::new(None),
            next_id: AtomicI64::new(1),
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut req = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(&jsonrpc_request(id, method, params));
        if let Some(tok) = &self.bearer {
            req = req.bearer_auth(tok);
        }
        if let Some(sid) = self.session_id.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", sid);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(McpError::Unauthorized);
        }
        // Capture session id minted on initialize.
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().unwrap() = Some(sid.to_string());
        }
        if !resp.status().is_success() {
            return Err(McpError::Transport(format!("HTTP {}", resp.status())));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        let events = parse_sse_events(&body);
        // Check for a JSON-RPC error in the matched event.
        if let Some(ev) = events
            .iter()
            .find(|e| e.get("id").and_then(|i| i.as_i64()) == Some(id))
            && let Some(err) = ev.get("error")
        {
            return Err(McpError::Rpc(err.to_string()));
        }
        Ok(jsonrpc_result_for(&events, id)
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Exchange the MCP `initialize` handshake.
    pub async fn initialize(&mut self) -> Result<InitializeInfo, McpError> {
        let res = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "mur-agent-runtime", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        // MCP lifecycle: send `notifications/initialized` after the response.
        let _ = self
            .request("notifications/initialized", serde_json::json!({}))
            .await;
        InitializeInfo::from_result(&res).map_err(McpError::Protocol)
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, McpError> {
        let res = self.request("tools/list", json!({})).await?;
        ToolInfo::list_from_result(&res).map_err(McpError::Protocol)
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Value, McpError> {
        self.request("tools/call", json!({"name": name, "arguments": args}))
            .await
    }

    /// Send an HTTP DELETE to terminate the session (best-effort).
    pub async fn shutdown(self) {
        if let Some(sid) = self.session_id.into_inner().unwrap() {
            let mut req = self
                .http
                .delete(&self.url)
                .header("Mcp-Session-Id", sid)
                .header("MCP-Protocol-Version", PROTOCOL_VERSION);
            if let Some(tok) = &self.bearer {
                req = req.bearer_auth(tok);
            }
            let _ = req.send().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_jsonrpc_request() {
        let r = jsonrpc_request(3, "tools/list", serde_json::json!({}));
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], 3);
        assert_eq!(r["method"], "tools/list");
    }
}
