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
use crate::sandbox::reqwest_guard::{GuardedHttpClient, HostGuard};

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Material needed to refresh an expired OAuth access token (RFC 6749 §6).
///
/// Carried by `HttpMcpClient` so that a 401 response can trigger one refresh
/// attempt and retry automatically.
pub struct RefreshCtx {
    pub token_endpoint: String,
    pub client_id: String,
    /// The resolved refresh token value (not a `SecretRef` — already resolved
    /// at connect time so no async I/O is needed in the hot request path).
    pub refresh_token: String,
    /// The `SecretRef` for the access token; used to persist the refreshed
    /// token back to the keychain so subsequent process restarts pick it up.
    pub access_token_ref: mur_common::secret::SecretRef,
}

pub struct HttpMcpClient {
    /// The only HTTP capability this client has: every POST, the session
    /// DELETE, every redirect hop and the OAuth refresh are host-checked
    /// against this entry's own set (#D6).
    http: GuardedHttpClient,
    url: String,
    /// Mutable under `&self` so the refresh-on-401 path can swap in a new token.
    bearer: Mutex<Option<String>>,
    session_id: Mutex<Option<String>>,
    next_id: AtomicI64,
    /// Refresh context; `None` when no OAuth refresh token is available.
    refresh: Option<RefreshCtx>,
}

/// The hosts one remote entry may reach: its `url` host, plus its OAuth
/// `token_endpoint` host when it has one. Frozen on the client at connect, so
/// one entry's bearer or refresh token cannot reach another entry's server.
/// An unparsable URL contributes nothing; the request itself then fails on it.
pub fn entry_hosts(url: &str, token_endpoint: Option<&str>) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for u in std::iter::once(url).chain(token_endpoint) {
        if let Some(h) = reqwest::Url::parse(u)
            .ok()
            .and_then(|p| p.host_str().map(str::to_string))
            && !hosts.iter().any(|x| x.eq_ignore_ascii_case(&h))
        {
            hosts.push(h);
        }
    }
    hosts
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
    /// `refresh` carries the material needed to refresh the token on a 401.
    /// `guard` is this entry's host set (see [`entry_hosts`]).
    pub async fn connect(
        url: &str,
        bearer: Option<String>,
        refresh: Option<RefreshCtx>,
        guard: HostGuard,
    ) -> Result<Self, McpError> {
        // `.no_proxy()`: a proxied request resolves the proxy's host, so the
        // guard would never see the real destination (#1297).
        let http = GuardedHttpClient::build(reqwest::Client::builder().no_proxy(), guard)
            .map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(Self {
            http,
            url: url.to_string(),
            bearer: Mutex::new(bearer),
            session_id: Mutex::new(None),
            next_id: AtomicI64::new(1),
            refresh,
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let mut retried = false;
        loop {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let mut req = self
                .http
                .post(&self.url)
                .map_err(McpError::Transport)?
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", PROTOCOL_VERSION)
                .json(&jsonrpc_request(id, method, params.clone()));
            if let Some(tok) = self.bearer.lock().unwrap().clone() {
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
                // Attempt one refresh if we have refresh material and haven't
                // already retried. Avoids an infinite refresh loop.
                if !retried && let Some(ctx) = &self.refresh {
                    match crate::oauth::refresh(
                        &self.http,
                        &ctx.token_endpoint,
                        &ctx.client_id,
                        &ctx.refresh_token,
                    )
                    .await
                    {
                        Ok(tokens) => {
                            // Swap in the new access token.
                            *self.bearer.lock().unwrap() = Some(tokens.access_token.clone());
                            // Best-effort: persist back to the keychain so
                            // subsequent restarts don't immediately expire.
                            // Never log the token value.
                            if let mur_common::secret::SecretRef::Keychain { service, account } =
                                &ctx.access_token_ref
                            {
                                // Ignore write errors (locked, sandbox, etc.)
                                let _ = mur_common::secret::keychain_set(
                                    service,
                                    account,
                                    &tokens.access_token,
                                )
                                .await;
                            }
                            retried = true;
                            continue; // retry the loop with the new token
                        }
                        Err(_) => {
                            // Refresh failed — return Unauthorized.
                            return Err(McpError::Unauthorized);
                        }
                    }
                }
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
            return Ok(jsonrpc_result_for(&events, id)
                .cloned()
                .unwrap_or(Value::Null));
        }
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
            let Ok(req) = self.http.delete(&self.url) else {
                return;
            };
            let mut req = req
                .header("Mcp-Session-Id", sid)
                .header("MCP-Protocol-Version", PROTOCOL_VERSION);
            if let Some(tok) = self.bearer.into_inner().unwrap() {
                req = req.bearer_auth(tok);
            }
            let _ = req.send().await;
        }
    }
}

#[cfg(test)]
#[path = "http_mcp_client_tests.rs"]
mod tests;
