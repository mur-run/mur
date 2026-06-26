# Remote MCP (Streamable HTTP + OAuth 2.1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a MUR agent use a **remote** MCP server (hosted over Streamable HTTP) — not just local stdio subprocesses — with a static bearer token first, and the full OAuth 2.1 + PKCE flow second.

**Architecture:** Today `McpClient` (`mur-agent-runtime/src/protocol/mcp_client.rs`) is stdio-only: it spawns a subprocess and speaks JSON-RPC over stdin/stdout. We turn `McpClient` into an **enum** over two transports — the existing stdio one (renamed `StdioMcpClient`) and a new `HttpMcpClient` (Streamable HTTP via `reqwest`). The transport is chosen from a new `McpServerEntry.url` field. Auth is a new `McpServerEntry.auth` field: Phase 1 a static bearer token resolved from a `SecretRef`; Phase 2 an OAuth 2.1 access/refresh token obtained via discovery → dynamic client registration → authorization-code-with-PKCE (localhost redirect), stored as Keychain `SecretRef`s and auto-refreshed on 401.

**Tech Stack:** Rust (edition 2024), `reqwest` (already a workspace dep in `mur-agent-runtime` and `mur-core`), `serde`/`serde_json`, `sha2` + `base64` (PKCE), `tokio` (localhost callback listener), the existing `SecretRef` (`mur-common/src/secret.rs`) + Keychain resolver.

## Global Constraints

- **MCP transport** = "Streamable HTTP" per MCP spec rev **2025-03-26** (single endpoint; POST JSON-RPC; response is `application/json` OR `text/event-stream`; `Mcp-Session-Id` response header on `initialize`, echoed on later requests). Send header `MCP-Protocol-Version: 2025-06-18` on every request after initialization.
- **MCP auth** = the MCP Authorization spec (rev 2025-06-18): the MCP server is an OAuth 2.1 **resource server**; discovery via RFC 9728 (`/.well-known/oauth-protected-resource`) → RFC 8414 (`/.well-known/oauth-authorization-server`); **Dynamic Client Registration** (RFC 7591); **Authorization Code + PKCE** (S256) with a `resource` parameter (RFC 8707).
- **Rust edition 2024** — `let` chains stable.
- **No hardcoded values** — endpoint URLs, ports, scopes come from discovery / config / args, never literals (the two `.well-known` suffixes and the `S256` method name are protocol constants, allowed).
- **Brand** — any user-facing string says **MUR** (uppercase); CLI/code identifiers stay lowercase `mur`.
- **Secrets never logged** — bearer/access/refresh tokens are `SecretRef`s; never `println!`/`tracing` their resolved values.
- **Backward compatible** — every new `McpServerEntry` field is `#[serde(default, skip_serializing_if = "Option::is_none")]`; existing stdio entries deserialize unchanged.
- **Run tests** with `ORT_STRATEGY=download cargo test -p <crate> <name>` (the onnxruntime link needs it); CI gate is `cargo clippy --no-deps -- -D warnings` + `cargo fmt`.

---

## File Structure

- `mur-common/src/agent.rs` — add `url` + `auth` to `McpServerEntry`; add `McpAuth` + `OauthAuth` enums/structs (Phase 1 schema; Phase 2 extends `OauthAuth`).
- `mur-common/src/secret.rs` — ensure `SecretRef` round-trips serde (string form); add `from_str`/`Display` if missing (needed so `McpAuth` serializes).
- `mur-agent-runtime/src/protocol/mcp_client.rs` — rename the existing struct to `StdioMcpClient`; add the `McpClient` enum + `connect()` dispatcher.
- `mur-agent-runtime/src/protocol/http_mcp_client.rs` — **new**: `HttpMcpClient` (Streamable HTTP). Holds the JSON-RPC framing, the POST, and the SSE response parser.
- `mur-agent-runtime/src/protocol/mcp_sse.rs` — **new**: pure SSE-event parser (`parse_sse_events`) + JSON-RPC id matching. Pure → fully TDD'd.
- `mur-agent-runtime/src/mcp/pool.rs:48` — `McpClient::spawn(...)` → `McpClient::connect(...)`.
- `mur-agent-runtime/src/oauth/mod.rs` — **new (Phase 2)**: discovery, dynamic client registration, PKCE, the localhost callback listener, token exchange + refresh.
- `mur-agent-runtime/src/oauth/pkce.rs` — **new (Phase 2)**: pure PKCE (`code_verifier` + `code_challenge`) + discovery-URL builders. Pure → TDD'd.
- `mur-core/src/cmd/agent/mcp.rs` — add `cmd_mcp_add_remote(...)`; reuse from registry-add.
- `mur-core/src/cmd/agent/mcp_registry.rs` — remote-only registry servers offer `add-remote` instead of erroring.
- `mur-core/src/cmd/agent/mcp_login.rs` — **new (Phase 2)**: `cmd_mcp_login(agent, name)` runs the OAuth flow and writes the token refs.
- `mur-core/src/cli/agent.rs` + `mur-core/src/dispatch.rs` — `AgentMcpAction::AddRemote` (Phase 1) and `::Login` (Phase 2).

---

# Phase 1 — Streamable HTTP transport + bearer token

Ships usable remote MCP for any server that accepts a static token (GitHub PAT, Linear, self-hosted, registry remote servers). OAuth (Phase 2) is only needed for servers that require the interactive dance.

## Task 1: Schema — `url` + `auth` on `McpServerEntry`

**Files:**
- Modify: `mur-common/src/agent.rs` (`McpServerEntry` struct; new `McpAuth` + `OauthAuth`)
- Modify: `mur-common/src/secret.rs` (serde round-trip for `SecretRef`)
- Test: inline `#[cfg(test)]` in `mur-common/src/agent.rs`

**Interfaces:**
- Produces: `McpServerEntry.url: Option<String>`, `McpServerEntry.auth: Option<McpAuth>`; `enum McpAuth { Bearer { token: SecretRef }, Oauth(OauthAuth) }`; `struct OauthAuth { token_endpoint: String, client_id: String, access_token: SecretRef, refresh_token: Option<SecretRef>, expires_at: u64 }`.

- [ ] **Step 1: Write the failing test** (append to `mur-common/src/agent.rs` tests)

```rust
#[test]
fn mcp_entry_roundtrips_remote_bearer() {
    let e = McpServerEntry {
        name: "gh".into(),
        command: String::new(),
        url: Some("https://api.example.com/mcp".into()),
        auth: Some(McpAuth::Bearer {
            token: crate::secret::SecretRef::Env("GH_TOKEN".into()),
        }),
        ..Default::default()
    };
    let y = serde_yaml::to_string(&e).unwrap();
    let back: McpServerEntry = serde_yaml::from_str(&y).unwrap();
    assert_eq!(back.url.as_deref(), Some("https://api.example.com/mcp"));
    assert!(matches!(back.auth, Some(McpAuth::Bearer { .. })));
    // A legacy stdio entry (no url/auth) still parses.
    let legacy: McpServerEntry =
        serde_yaml::from_str("name: fs\ncommand: npx\nargs: [\"-y\",\"fs\"]\n").unwrap();
    assert!(legacy.url.is_none() && legacy.auth.is_none());
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-common mcp_entry_roundtrips_remote_bearer`
Expected: FAIL — `McpServerEntry` has no field `url`/`auth`, `McpAuth` undefined.

- [ ] **Step 3: Add the fields + enums**

In `McpServerEntry` (after `args`):

```rust
    /// Remote MCP endpoint (Streamable HTTP). When set, this is an
    /// HTTP-transport server and `command`/`args` are unused. (Remote MCP)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Auth for a remote (`url`) server. `None` = no auth header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<McpAuth>,
```

New types (same file):

```rust
/// How to authenticate to a remote MCP server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpAuth {
    /// Static bearer token, resolved from a `SecretRef` (Keychain/env/file/cmd).
    Bearer { token: crate::secret::SecretRef },
    /// OAuth 2.1 — tokens obtained via the login flow, stored as `SecretRef`s.
    Oauth(OauthAuth),
}

/// OAuth 2.1 state persisted alongside a remote MCP entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OauthAuth {
    /// Authorization-server token endpoint (from discovery).
    pub token_endpoint: String,
    /// Client id from dynamic client registration.
    pub client_id: String,
    /// Keychain ref to the access token.
    pub access_token: crate::secret::SecretRef,
    /// Keychain ref to the refresh token, if the server issued one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<crate::secret::SecretRef>,
    /// Unix-epoch seconds the access token expires (0 = unknown).
    #[serde(default)]
    pub expires_at: u64,
}
```

- [ ] **Step 4: Ensure `SecretRef` serializes** (if it lacks `Serialize`/`Deserialize`)

`McpAuth` derives serde, so `SecretRef` must too. If `mur-common/src/secret.rs`'s `SecretRef` has no `#[derive(Serialize, Deserialize)]`, give it a string form and serde via that string (do NOT hand-expand variants):

```rust
impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretRef::Env(v) => write!(f, "env:{v}"),
            SecretRef::Keychain { service, account } => write!(f, "keychain:{service}/{account}"),
            SecretRef::File(p) => write!(f, "file:{}", p.display()),
            SecretRef::Cmd(c) => write!(f, "cmd:{c}"),
        }
    }
}
impl std::str::FromStr for SecretRef {
    type Err = SecretError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(v) = s.strip_prefix("env:") { return Ok(SecretRef::Env(v.into())); }
        if let Some(v) = s.strip_prefix("keychain:") {
            let (service, account) = v.split_once('/').ok_or(SecretError::Malformed)?;
            return Ok(SecretRef::Keychain { service: service.into(), account: account.into() });
        }
        if let Some(v) = s.strip_prefix("file:") { return Ok(SecretRef::File(v.into())); }
        if let Some(v) = s.strip_prefix("cmd:") { return Ok(SecretRef::Cmd(v.into())); }
        Err(SecretError::Malformed)
    }
}
// serde via the string form:
impl serde::Serialize for SecretRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
impl<'de> serde::Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
```

> If `SecretRef` already round-trips serde (the model registry stores it in `models.yaml`, so it likely does), **skip this step** — just confirm `mur-common/src/model.rs` uses the same `SecretRef` and that `SecretError` has (or gains) a `Malformed` variant.

- [ ] **Step 5: Run the test, verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-common mcp_entry_roundtrips_remote_bearer`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/agent.rs mur-common/src/secret.rs
git commit -m "feat(mcp): McpServerEntry.url + auth (bearer/oauth) schema for remote MCP"
```

## Task 2: SSE response parser (pure)

A Streamable HTTP response is either one JSON object or a `text/event-stream` body. For request/response tool calls the stream carries our reply (and possibly notifications) then closes. Parse the events and pick the JSON-RPC message whose `id` matches our request.

**Files:**
- Create: `mur-agent-runtime/src/protocol/mcp_sse.rs`
- Modify: `mur-agent-runtime/src/protocol/mod.rs` (add `pub mod mcp_sse;`)
- Test: inline in `mcp_sse.rs`

**Interfaces:**
- Produces: `fn parse_sse_events(body: &str) -> Vec<serde_json::Value>` (each `data:` payload parsed as JSON, non-JSON `data:` skipped); `fn jsonrpc_result_for(events: &[serde_json::Value], id: i64) -> Option<&serde_json::Value>` (returns the `result` of the response whose `id` matches, or surfaces `error`).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_sse_and_matches_id() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/log\",\"params\":{}}\n\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 2);
        let r = jsonrpc_result_for(&events, 1).unwrap();
        assert!(r.get("tools").is_some());
        assert!(jsonrpc_result_for(&events, 99).is_none());
    }
    #[test]
    fn plain_json_body_is_one_event() {
        let body = "{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}";
        let events = parse_sse_events(body);
        assert_eq!(jsonrpc_result_for(&events, 7).unwrap()["ok"], true);
    }
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime parses_sse_and_matches_id`
Expected: FAIL — `parse_sse_events`/`jsonrpc_result_for` undefined.

- [ ] **Step 3: Implement**

```rust
//! Pure parsing for Streamable HTTP MCP responses (JSON or SSE).
use serde_json::Value;

/// Extract JSON payloads from an MCP HTTP response body. Handles both a bare
/// JSON object and a `text/event-stream` body (one or more `data:` lines per
/// event, events separated by blank lines). Non-JSON `data:` lines are skipped.
pub fn parse_sse_events(body: &str) -> Vec<Value> {
    let trimmed = body.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return vec![v];
        }
    }
    let mut out = Vec::new();
    for block in body.split("\n\n") {
        let data: String = block
            .lines()
            .filter_map(|l| l.strip_prefix("data:").map(|d| d.trim_start()))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            out.push(v);
        }
    }
    out
}

/// Return the `result` of the JSON-RPC response whose `id` matches, mapping a
/// JSON-RPC `error` object into an `Err`.
pub fn jsonrpc_result_for(events: &[Value], id: i64) -> Option<&Value> {
    events
        .iter()
        .find(|e| e.get("id").and_then(|i| i.as_i64()) == Some(id))
        .map(|e| &e["result"])
}
```

> Note: `jsonrpc_result_for` returns `&e["result"]` (Null if the message carried `error` instead). Task 3's caller checks for an `error` member before using the result and surfaces it. `// ponytail: full-body read; server-initiated sampling mid-call is out of scope — add a streaming reader if a server needs it.`

- [ ] **Step 4: Run the test, verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime parses_sse`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/protocol/mcp_sse.rs mur-agent-runtime/src/protocol/mod.rs
git commit -m "feat(mcp): SSE/JSON response parser for Streamable HTTP transport"
```

## Task 3: `HttpMcpClient` (Streamable HTTP)

**Files:**
- Create: `mur-agent-runtime/src/protocol/http_mcp_client.rs`
- Modify: `mur-agent-runtime/src/protocol/mod.rs` (`pub mod http_mcp_client;`)
- Test: inline (a JSON-RPC framing unit test; the network path is verified live in Task 6)

**Interfaces:**
- Consumes: `parse_sse_events`, `jsonrpc_result_for` (Task 2); `InitializeInfo`, `ToolInfo`, `McpError` (existing in `mcp_client.rs` — re-export or import).
- Produces: `HttpMcpClient::connect(url: &str, bearer: Option<String>) -> Result<Self, McpError>`; async `initialize(&mut self) -> Result<InitializeInfo, McpError>`; `list_tools(&self) -> Result<Vec<ToolInfo>, McpError>`; `call_tool(&self, name: &str, args: Value) -> Result<Value, McpError>`; `shutdown(self)`. Helper `fn jsonrpc_request(id: i64, method: &str, params: Value) -> Value`.

- [ ] **Step 1: Write the failing test** (pure framing)

```rust
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
```

- [ ] **Step 2: Run it, verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime builds_jsonrpc_request`
Expected: FAIL — module/function undefined.

- [ ] **Step 3: Implement**

```rust
//! Streamable HTTP MCP transport (MCP spec 2025-03-26): one endpoint, POST
//! JSON-RPC, response is JSON or text/event-stream, session id via header.
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};

use super::mcp_client::{InitializeInfo, McpError, ToolInfo};
use super::mcp_sse::{jsonrpc_result_for, parse_sse_events};

const PROTOCOL_VERSION: &str = "2025-06-18";

pub struct HttpMcpClient {
    http: reqwest::Client,
    url: String,
    bearer: Option<String>,
    session_id: std::sync::Mutex<Option<String>>,
    next_id: AtomicI64,
}

pub fn jsonrpc_request(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

impl HttpMcpClient {
    pub async fn connect(url: &str, bearer: Option<String>) -> Result<Self, McpError> {
        Ok(Self {
            http: reqwest::Client::builder()
                .build()
                .map_err(|e| McpError::Transport(e.to_string()))?,
            url: url.to_string(),
            bearer,
            session_id: std::sync::Mutex::new(None),
            next_id: AtomicI64::new(1),
        })
    }

    /// POST one JSON-RPC request and return its `result` (or a JSON-RPC error).
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
        let resp = req.send().await.map_err(|e| McpError::Transport(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(McpError::Unauthorized);
        }
        // Capture the session id minted on initialize.
        if let Some(sid) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *self.session_id.lock().unwrap() = Some(sid.to_string());
        }
        if !resp.status().is_success() {
            return Err(McpError::Transport(format!("HTTP {}", resp.status())));
        }
        let body = resp.text().await.map_err(|e| McpError::Transport(e.to_string()))?;
        let events = parse_sse_events(&body);
        let msg = events
            .iter()
            .find(|e| e.get("id").and_then(|i| i.as_i64()) == Some(id))
            .ok_or_else(|| McpError::Transport("no response for request id".into()))?;
        if let Some(err) = msg.get("error") {
            return Err(McpError::Rpc(err.to_string()));
        }
        Ok(jsonrpc_result_for(&events, id).cloned().unwrap_or(Value::Null))
    }

    pub async fn initialize(&mut self) -> Result<InitializeInfo, McpError> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "mur", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        // Per spec, follow with the initialized notification (best-effort).
        let _ = self.notify("notifications/initialized", json!({})).await;
        InitializeInfo::from_result(&result).map_err(|e| McpError::Protocol(e.to_string()))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let mut req = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
        if let Some(tok) = &self.bearer {
            req = req.bearer_auth(tok);
        }
        if let Some(sid) = self.session_id.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", sid);
        }
        req.send().await.map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(())
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, McpError> {
        let result = self.request("tools/list", json!({})).await?;
        ToolInfo::list_from_result(&result).map_err(|e| McpError::Protocol(e.to_string()))
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        self.request("tools/call", json!({"name": name, "arguments": arguments})).await
    }

    pub async fn shutdown(self) {
        // Stateless HTTP — best-effort session delete.
        if let Some(sid) = self.session_id.lock().unwrap().clone() {
            let _ = self.http.delete(&self.url).header("Mcp-Session-Id", sid).send().await;
        }
    }
}
```

> This relies on helpers `InitializeInfo::from_result`, `ToolInfo::list_from_result`, and `McpError` variants `Transport`, `Rpc`, `Protocol`, `Unauthorized`. If the existing stdio client builds these inline, **extract** them into `impl InitializeInfo`/`impl ToolInfo` constructors + add the missing `McpError` variants in `mcp_client.rs` as part of this task (pure refactor, no behavior change for stdio). `// ponytail: reuse the stdio client's existing JSON→type code — don't write a second parser.`

- [ ] **Step 4: Run the test, verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime builds_jsonrpc_request`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/protocol/http_mcp_client.rs mur-agent-runtime/src/protocol/mod.rs mur-agent-runtime/src/protocol/mcp_client.rs
git commit -m "feat(mcp): HttpMcpClient — Streamable HTTP transport (POST + SSE + session id)"
```

## Task 4: `McpClient` enum + `connect()` dispatcher

**Files:**
- Modify: `mur-agent-runtime/src/protocol/mcp_client.rs` (rename struct → `StdioMcpClient`; add `enum McpClient`)
- Modify: `mur-agent-runtime/src/mcp/pool.rs:48`
- Test: inline transport-selection test

**Interfaces:**
- Consumes: `StdioMcpClient` (renamed), `HttpMcpClient` (Task 3).
- Produces: `enum McpClient { Stdio(StdioMcpClient), Http(HttpMcpClient) }` with async `connect(entry, policy, proxy)`, `initialize`, `list_tools`, `call_tool`, `shutdown` dispatching by variant. `connect` picks HTTP iff `entry.url.is_some()`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn connect_picks_http_for_url_entry() {
    let mut entry = McpServerEntry::default();
    entry.name = "r".into();
    entry.url = Some("https://example.invalid/mcp".into());
    let client = McpClient::connect(&entry, &SandboxPolicy::default(), None).await.unwrap();
    assert!(matches!(client, McpClient::Http(_)));
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime connect_picks_http_for_url_entry`
Expected: FAIL — `McpClient::connect` / enum doesn't exist (struct still named `McpClient`).

- [ ] **Step 3: Rename + add the enum**

Rename the existing `pub struct McpClient` and all its `impl McpClient` to `StdioMcpClient` (its `spawn` stays). Then add:

```rust
/// A connected MCP server over either transport.
pub enum McpClient {
    Stdio(StdioMcpClient),
    Http(super::http_mcp_client::HttpMcpClient),
}

impl McpClient {
    /// Connect to `entry`: Streamable HTTP iff it has a `url`, else spawn stdio.
    pub async fn connect(
        entry: &McpServerEntry,
        policy: &SandboxPolicy,
        proxy: Option<&ProxyConfig>,
    ) -> Result<Self, McpError> {
        match &entry.url {
            Some(url) => {
                let bearer = resolve_bearer(entry).await?;
                Ok(McpClient::Http(
                    super::http_mcp_client::HttpMcpClient::connect(url, bearer).await?,
                ))
            }
            None => Ok(McpClient::Stdio(StdioMcpClient::spawn(entry, policy, proxy).await?)),
        }
    }
    pub async fn initialize(&mut self) -> Result<InitializeInfo, McpError> {
        match self {
            McpClient::Stdio(c) => c.initialize().await,
            McpClient::Http(c) => c.initialize().await,
        }
    }
    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, McpError> {
        match self {
            McpClient::Stdio(c) => c.list_tools().await,
            McpClient::Http(c) => c.list_tools().await,
        }
    }
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Value, McpError> {
        match self {
            McpClient::Stdio(c) => c.call_tool(name, args).await,
            McpClient::Http(c) => c.call_tool(name, args).await,
        }
    }
    pub async fn shutdown(self) {
        match self {
            McpClient::Stdio(c) => c.shutdown().await,
            McpClient::Http(c) => c.shutdown().await,
        }
    }
}

/// Resolve a Phase-1 bearer token (`McpAuth::Bearer`) from its `SecretRef`.
/// Phase 2 (`Oauth`) is resolved in Task 9.
async fn resolve_bearer(entry: &McpServerEntry) -> Result<Option<String>, McpError> {
    match &entry.auth {
        Some(McpAuth::Bearer { token }) => Ok(Some(
            token.resolve().map_err(|e| McpError::Transport(e.to_string()))?,
        )),
        _ => Ok(None),
    }
}
```

> `SecretRef::resolve()` is the existing resolver (env/keychain/file/cmd). If it lives in `mur-core` rather than `mur-common`, pass the already-resolved token into the runtime via the entry-loading path, or move the resolver to `mur-common` (it has no I/O beyond keychain). Confirm where `SecretRef` is resolved today and reuse it.

- [ ] **Step 4: Update the pool call site** (`mur-agent-runtime/src/mcp/pool.rs:48`)

```rust
let mut client = McpClient::connect(entry, &self.policy, self.proxy.as_ref()).await?;
```

- [ ] **Step 5: Run the test + the existing pool tests**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime connect_picks_http_for_url_entry && ORT_STRATEGY=download cargo test -p mur-agent-runtime mcp::`
Expected: PASS (transport selection + existing stdio tests still green after the rename).

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/protocol/mcp_client.rs mur-agent-runtime/src/mcp/pool.rs
git commit -m "feat(mcp): McpClient enum over stdio + http transports; pool uses connect()"
```

## Task 5: CLI `mur agent mcp add-remote`

**Files:**
- Modify: `mur-core/src/cli/agent.rs` (`AgentMcpAction::AddRemote`)
- Modify: `mur-core/src/cmd/agent/mcp.rs` (`cmd_mcp_add_remote`)
- Modify: `mur-core/src/dispatch.rs` (dispatch arm)
- Test: inline test that the entry is written with url + bearer auth

**Interfaces:**
- Produces: `cmd_mcp_add_remote(agent: &str, name: &str, url: &str, bearer: Option<SecretRef>) -> Result<()>` — appends an `McpServerEntry { name, url: Some(url), auth: bearer.map(|t| McpAuth::Bearer{token:t}), .. }` (no binary pin; remote has no local binary). CLI: `AddRemote { name, server_name, url, bearer_env: Option<String>, bearer_keychain: Option<String> }`.

- [ ] **Step 1: Write the failing test** (in `mcp.rs` tests, using a temp `MUR_HOME`)

```rust
#[test]
fn add_remote_writes_url_and_bearer() {
    let tmp = tempfile::TempDir::new().unwrap();
    // (set MUR_HOME to tmp + create the agent profile as the other mcp tests do)
    cmd_mcp_add_remote("alice", "gh", "https://api.example.com/mcp",
        Some(mur_common::secret::SecretRef::Env("GH_TOKEN".into()))).unwrap();
    let (_p, profile) = load_profile_for_edit("alice").unwrap();
    let e = profile.mcp_servers.iter().find(|m| m.name == "gh").unwrap();
    assert_eq!(e.url.as_deref(), Some("https://api.example.com/mcp"));
    assert!(matches!(e.auth, Some(mur_common::agent::McpAuth::Bearer { .. })));
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core add_remote_writes_url_and_bearer`
Expected: FAIL — `cmd_mcp_add_remote` undefined.

- [ ] **Step 3: Implement `cmd_mcp_add_remote`** (mirror `cmd_mcp_add` minus the binary pin)

```rust
/// Add a remote (Streamable HTTP) MCP server to `agent`. No binary pin — the
/// server runs elsewhere; trust comes from the URL + bearer/OAuth auth.
pub fn cmd_mcp_add_remote(
    agent: &str,
    name: &str,
    url: &str,
    bearer: Option<mur_common::secret::SecretRef>,
) -> anyhow::Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    if profile.mcp_servers.iter().any(|m| m.name == name) {
        anyhow::bail!("MCP server '{name}' already exists on '{agent}'; remove it first");
    }
    profile.mcp_servers.push(mur_common::agent::McpServerEntry {
        name: name.to_string(),
        url: Some(url.to_string()),
        auth: bearer.map(|token| mur_common::agent::McpAuth::Bearer { token }),
        ..Default::default()
    });
    save_profile(&path, &profile)?;
    println!("Added remote MCP server '{name}' → {url} for agent '{agent}'.");
    Ok(())
}
```

- [ ] **Step 4: Wire the CLI variant + dispatch**

`AgentMcpAction` (cli/agent.rs):

```rust
    /// Add a remote (Streamable HTTP) MCP server by URL.
    AddRemote {
        name: String,
        server_name: String,
        url: String,
        /// Bearer token from an env var.
        #[arg(long)]
        bearer_env: Option<String>,
        /// Bearer token from the keychain (`service/account`).
        #[arg(long)]
        bearer_keychain: Option<String>,
    },
```

dispatch.rs (after the other mcp arms):

```rust
    AgentMcpAction::AddRemote { name, server_name, url, bearer_env, bearer_keychain } => {
        let bearer = match (bearer_env, bearer_keychain) {
            (Some(v), _) => Some(mur_common::secret::SecretRef::Env(v)),
            (_, Some(sa)) => {
                let (service, account) = sa.split_once('/').ok_or_else(||
                    anyhow::anyhow!("--bearer-keychain expects service/account"))?;
                Some(mur_common::secret::SecretRef::Keychain {
                    service: service.into(), account: account.into() })
            }
            _ => None,
        };
        cmd::agent::mcp::cmd_mcp_add_remote(&name, &server_name, &url, bearer)?
    }
```

- [ ] **Step 5: Run the test, verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core add_remote_writes_url_and_bearer`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent/mcp.rs mur-core/src/dispatch.rs
git commit -m "feat(mcp): mur agent mcp add-remote <agent> <name> <url> [--bearer-*]"
```

## Task 6: Registry-add remote servers + live verify

**Files:**
- Modify: `mur-core/src/cmd/agent/mcp_registry.rs` (`cmd_mcp_registry_add` remote branch)

**Interfaces:**
- Consumes: `cmd_mcp_add_remote` (Task 5), `RegistryServer.remotes` (already parsed).

- [ ] **Step 1: Change the remote branch** — instead of erroring, install the remote URL

In `cmd_mcp_registry_add`, replace the `remotes`-not-empty error with:

```rust
    if let Some((command, args)) = srv.packages.iter().find_map(package_command) {
        let id = short_id(server_name);
        return cmd_mcp_add(agent, &id, &command, &args, McpAddPin {
            publisher_name: Some("mcp-registry".to_string()),
            publisher_registry_id: Some(server_name.to_string()),
            ..Default::default()
        });
    }
    if let Some(remote) = srv.remotes.first() {
        let id = short_id(server_name);
        println!("'{server_name}' is a remote MCP server ({}). Installing by URL.", remote.r#type);
        println!("If it needs OAuth, run:  mur agent mcp login {agent} {id}");
        return super::mcp::cmd_mcp_add_remote(agent, &id, &remote.url, None);
    }
    anyhow::bail!("'{server_name}' has no installable package or remote endpoint")
```

(Extract the `server_name.rsplit('/').next()...replace('.', "-")` into a `fn short_id(name: &str) -> String` so both branches share it.)

- [ ] **Step 2: Build + clippy**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core --no-deps -- -D warnings`
Expected: `Finished`, no warnings.

- [ ] **Step 3: Live verify (manual)** — needs a reachable test server

```bash
# A public no-auth Streamable HTTP MCP server (e.g. an `everything`/echo demo),
# or a local one: npx @modelcontextprotocol/server-everything streamableHttp
cargo run -q --bin mur -- agent mcp add-remote dataml echo http://127.0.0.1:3001/mcp
# then start the agent runtime and confirm the tool list loads from the remote:
mur agent mcp list dataml          # shows echo [remote]
# (full tool-call check happens once the runtime dials it)
mur agent mcp remove dataml echo   # cleanup
```

Expected: the entry is written; when the runtime connects, `HttpMcpClient::initialize` + `list_tools` succeed against the remote.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/mcp_registry.rs
git commit -m "feat(mcp): registry-add installs remote servers by URL (login hint for OAuth)"
```

**Phase 1 done:** remote MCP works for token-auth servers. PR it as "Remote MCP (Streamable HTTP + bearer)".

---

# Phase 2 — OAuth 2.1 (discovery → DCR → PKCE → token)

For servers that return `401` with a `WWW-Authenticate` pointing at an OAuth resource. Adds `mur agent mcp login <agent> <name>`.

## Task 7: PKCE + discovery URL builders (pure)

**Files:**
- Create: `mur-agent-runtime/src/oauth/pkce.rs`
- Create: `mur-agent-runtime/src/oauth/mod.rs` (`pub mod pkce;` for now)
- Modify: `mur-agent-runtime/src/lib.rs` (`pub mod oauth;`)
- Test: inline

**Interfaces:**
- Produces: `fn code_verifier(seed: &[u8]) -> String` (43–128 char unreserved); `fn code_challenge(verifier: &str) -> String` (base64url(SHA256(verifier)), no padding); `fn protected_resource_url(server: &str) -> String` (`<origin>/.well-known/oauth-protected-resource`); `fn as_metadata_url(issuer: &str) -> String` (`<issuer>/.well-known/oauth-authorization-server`).

- [ ] **Step 1: Write the failing test** — use the RFC 7636 reference vector

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_matches_rfc7636_vector() {
        // RFC 7636 Appendix B verifier → challenge.
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(code_challenge(v), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }
    #[test]
    fn discovery_urls() {
        assert_eq!(protected_resource_url("https://mcp.example.com/sse"),
                   "https://mcp.example.com/.well-known/oauth-protected-resource");
        assert_eq!(as_metadata_url("https://auth.example.com"),
                   "https://auth.example.com/.well-known/oauth-authorization-server");
    }
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime pkce_matches_rfc7636_vector`
Expected: FAIL — functions undefined.

- [ ] **Step 3: Implement** (uses `sha2` + `base64` — add to `mur-agent-runtime/Cargo.toml` if absent; both are common workspace deps)

```rust
//! Pure PKCE + OAuth discovery-URL helpers (RFC 7636 / 8414 / 9728).
use base64::Engine;
use sha2::{Digest, Sha256};

const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// A high-entropy code verifier (43+ chars) from random `seed` bytes.
pub fn code_verifier(seed: &[u8]) -> String {
    seed.iter().map(|b| UNRESERVED[*b as usize % UNRESERVED.len()] as char).collect()
}

/// base64url(SHA256(verifier)) without padding (the S256 challenge).
pub fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// RFC 9728 protected-resource metadata URL for an MCP server URL.
pub fn protected_resource_url(server: &str) -> String {
    let origin = origin_of(server);
    format!("{origin}/.well-known/oauth-protected-resource")
}

/// RFC 8414 authorization-server metadata URL for an issuer.
pub fn as_metadata_url(issuer: &str) -> String {
    format!("{}/.well-known/oauth-authorization-server", issuer.trim_end_matches('/'))
}

fn origin_of(url: &str) -> String {
    // scheme://host[:port] — strip any path. ponytail: string split, no url crate.
    match url.find("://") {
        Some(i) => {
            let rest = &url[i + 3..];
            let host = rest.split('/').next().unwrap_or(rest);
            format!("{}://{}", &url[..i], host)
        }
        None => url.to_string(),
    }
}
```

> The caller seeds `code_verifier` with 32–64 random bytes (`rand`). `Math.random` bans don't apply (this is the runtime, not a workflow script).

- [ ] **Step 4: Run the test, verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime pkce_matches_rfc7636_vector discovery_urls`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/oauth/ mur-agent-runtime/src/lib.rs mur-agent-runtime/Cargo.toml
git commit -m "feat(oauth): pure PKCE (S256) + RFC 8414/9728 discovery URL builders"
```

## Task 8: OAuth metadata + dynamic client registration

**Files:**
- Modify: `mur-agent-runtime/src/oauth/mod.rs`
- Test: inline parse test

**Interfaces:**
- Produces: `struct AsMetadata { authorization_endpoint, token_endpoint, registration_endpoint: Option<String> }` (serde); `fn parse_as_metadata(json: &str) -> Result<AsMetadata>`; async `discover(http, server_url) -> Result<AsMetadata>` (protected-resource → issuer → AS metadata, with the documented fallback to `<origin>` as issuer); async `register_client(http, registration_endpoint, redirect_uri) -> Result<String /*client_id*/>` (RFC 7591 POST).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn parses_as_metadata() {
    let j = r#"{"authorization_endpoint":"https://a/x","token_endpoint":"https://a/t","registration_endpoint":"https://a/r"}"#;
    let m = parse_as_metadata(j).unwrap();
    assert_eq!(m.token_endpoint, "https://a/t");
    assert_eq!(m.registration_endpoint.as_deref(), Some("https://a/r"));
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime parses_as_metadata`
Expected: FAIL.

- [ ] **Step 3: Implement** (parse pure + the two async glue fns)

```rust
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AsMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
}

pub fn parse_as_metadata(json: &str) -> Result<AsMetadata> {
    Ok(serde_json::from_str(json)?)
}

#[derive(Deserialize)]
struct ProtectedResource {
    #[serde(default)]
    authorization_servers: Vec<String>,
}

/// Resolve the authorization server for an MCP server URL: protected-resource
/// metadata → issuer → AS metadata. Falls back to treating the server's origin
/// as the issuer when the server doesn't publish protected-resource metadata.
pub async fn discover(http: &reqwest::Client, server_url: &str) -> Result<AsMetadata> {
    let issuer = match http
        .get(pkce::protected_resource_url(server_url))
        .send()
        .await
        .ok()
        .filter(|r| r.status().is_success())
    {
        Some(r) => {
            let pr: ProtectedResource = r.json().await?;
            pr.authorization_servers
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("no authorization_servers in protected-resource metadata"))?
        }
        None => pkce::origin_of_pub(server_url),
    };
    let body = http.get(pkce::as_metadata_url(&issuer)).send().await?.text().await?;
    parse_as_metadata(&body)
}

/// RFC 7591 dynamic client registration → returns the issued client_id.
pub async fn register_client(
    http: &reqwest::Client,
    registration_endpoint: &str,
    redirect_uri: &str,
) -> Result<String> {
    #[derive(Deserialize)]
    struct Reg { client_id: String }
    let reg: Reg = http
        .post(registration_endpoint)
        .json(&serde_json::json!({
            "client_name": "MUR",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await?
        .json()
        .await?;
    Ok(reg.client_id)
}
```

> Expose `origin_of` from `pkce` as `pub fn origin_of_pub` (or move `origin_of` to `mod.rs`) so `discover` can reuse it. `// ponytail: one origin parser, shared.`

- [ ] **Step 4: Run the test, verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime parses_as_metadata`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/oauth/mod.rs mur-agent-runtime/src/oauth/pkce.rs
git commit -m "feat(oauth): AS metadata discovery + dynamic client registration (RFC 8414/7591)"
```

## Task 9: Authorization-code + PKCE flow (localhost callback) + token storage

**Files:**
- Modify: `mur-agent-runtime/src/oauth/mod.rs` (`run_authorization_flow`, `exchange_code`, `refresh`)
- Create: `mur-core/src/cmd/agent/mcp_login.rs` (`cmd_mcp_login`)
- Modify: `mur-core/src/cli/agent.rs` + `dispatch.rs` (`AgentMcpAction::Login`)

**Interfaces:**
- Consumes: `discover`, `register_client` (Task 8), `code_verifier`/`code_challenge` (Task 7).
- Produces: `struct Tokens { access_token, refresh_token: Option<String>, expires_in: u64 }`; async `run_authorization_flow(http, meta, client_id, server_url, redirect_port) -> Result<Tokens>`; async `exchange_code(...)`/`refresh(http, token_endpoint, client_id, refresh_token) -> Result<Tokens>`; `cmd_mcp_login(agent, name) -> Result<()>` writes the `OauthAuth` onto the entry + stores tokens as Keychain `SecretRef`s.

- [ ] **Step 1: Token-response parse test** (the only pure-testable bit; the browser+listener is integration)

```rust
#[test]
fn parses_token_response() {
    let j = r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600,"token_type":"Bearer"}"#;
    let t: Tokens = parse_tokens(j).unwrap();
    assert_eq!(t.access_token, "AT");
    assert_eq!(t.refresh_token.as_deref(), Some("RT"));
    assert_eq!(t.expires_in, 3600);
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime parses_token_response`
Expected: FAIL.

- [ ] **Step 3: Implement the flow**

```rust
#[derive(Debug, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: u64,
}

pub fn parse_tokens(json: &str) -> Result<Tokens> {
    Ok(serde_json::from_str(json)?)
}

/// Run authorization-code + PKCE: bind a localhost listener on `redirect_port`,
/// open the browser at the authorization endpoint, wait for the `?code=`
/// redirect, then exchange the code for tokens. `resource` = the MCP server URL
/// (RFC 8707).
pub async fn run_authorization_flow(
    http: &reqwest::Client,
    meta: &AsMetadata,
    client_id: &str,
    server_url: &str,
    redirect_port: u16,
) -> Result<Tokens> {
    use rand::RngCore;
    let mut seed = [0u8; 48];
    rand::thread_rng().fill_bytes(&mut seed);
    let verifier = pkce::code_verifier(&seed);
    let challenge = pkce::code_challenge(&verifier);
    let redirect_uri = format!("http://127.0.0.1:{redirect_port}/callback");

    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&resource={}",
        meta.authorization_endpoint,
        urlencoding::encode(client_id),
        urlencoding::encode(&redirect_uri),
        challenge,
        urlencoding::encode(server_url),
    );
    println!("Opening browser to authorize…\n  {auth_url}");
    let _ = open::that(&auth_url); // ponytail: `open` crate; if absent, print the URL to paste.

    let code = wait_for_code(redirect_port).await?;
    exchange_code(http, &meta.token_endpoint, client_id, &code, &verifier, &redirect_uri, server_url).await
}

/// Bind 127.0.0.1:port, accept one request, return its `?code=` query value.
async fn wait_for_code(port: u16) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let (mut sock, _) = listener.accept().await?;
    let mut buf = [0u8; 2048];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    // First line: GET /callback?code=XYZ&... HTTP/1.1
    let target = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
    let code = target
        .split_once("code=")
        .and_then(|(_, q)| q.split('&').next())
        .map(|c| c.to_string())
        .ok_or_else(|| anyhow::anyhow!("authorization redirect had no ?code="))?;
    let _ = sock
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<h2>MUR: authorized. You can close this tab.</h2>")
        .await;
    Ok(urlencoding::decode(&code).map(|c| c.into_owned()).unwrap_or(code))
}

async fn exchange_code(
    http: &reqwest::Client, token_endpoint: &str, client_id: &str, code: &str,
    verifier: &str, redirect_uri: &str, resource: &str,
) -> Result<Tokens> {
    let body = http
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", verifier),
            ("resource", resource),
        ])
        .send().await?.text().await?;
    parse_tokens(&body)
}

/// Refresh an expired access token. Used by `HttpMcpClient` on a 401.
pub async fn refresh(
    http: &reqwest::Client, token_endpoint: &str, client_id: &str, refresh_token: &str,
) -> Result<Tokens> {
    let body = http
        .post(token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send().await?.text().await?;
    parse_tokens(&body)
}
```

> Deps to confirm/add in `mur-agent-runtime/Cargo.toml`: `rand`, `urlencoding` (or hand-roll percent-encoding for the few chars), `open` (browser launch; or just print the URL). All small, common. `redirect_port`: pick a fixed loopback port from config (e.g. `MUR_OAUTH_REDIRECT_PORT`, default a documented constant) — must match the DCR `redirect_uri`.

- [ ] **Step 4: `cmd_mcp_login`** (`mur-core/src/cmd/agent/mcp_login.rs`) — orchestrate + persist

```rust
/// Run the OAuth login flow for a remote MCP entry and store its tokens.
pub async fn cmd_mcp_login(agent: &str, name: &str) -> anyhow::Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    let entry = profile.mcp_servers.iter_mut().find(|m| m.name == name)
        .ok_or_else(|| anyhow::anyhow!("no MCP server '{name}' on '{agent}'"))?;
    let url = entry.url.clone()
        .ok_or_else(|| anyhow::anyhow!("'{name}' is not a remote (url) server"))?;

    let http = reqwest::Client::new();
    let meta = mur_agent_runtime::oauth::discover(&http, &url).await?;
    let port = oauth_redirect_port(); // config/env, documented default
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let client_id = match &meta.registration_endpoint {
        Some(reg) => mur_agent_runtime::oauth::register_client(&http, reg, &redirect_uri).await?,
        None => anyhow::bail!("server's authorization server has no dynamic registration endpoint; manual client_id config not yet supported"),
    };
    let tokens = mur_agent_runtime::oauth::run_authorization_flow(&http, &meta, &client_id, &url, port).await?;

    // Store tokens in the keychain; reference them from the entry.
    let svc = format!("mur-mcp-{agent}");
    keychain_set(&svc, &format!("{name}.access"), &tokens.access_token)?;
    let refresh_ref = match &tokens.refresh_token {
        Some(rt) => { keychain_set(&svc, &format!("{name}.refresh"), rt)?;
            Some(mur_common::secret::SecretRef::Keychain { service: svc.clone(), account: format!("{name}.refresh") }) }
        None => None,
    };
    entry.auth = Some(mur_common::agent::McpAuth::Oauth(mur_common::agent::OauthAuth {
        token_endpoint: meta.token_endpoint,
        client_id,
        access_token: mur_common::secret::SecretRef::Keychain { service: svc, account: format!("{name}.access") },
        refresh_token: refresh_ref,
        expires_at: now_epoch() + tokens.expires_in,
    }));
    save_profile(&path, &profile)?;
    println!("Authorized '{name}' for agent '{agent}'.");
    Ok(())
}
```

> Reuse the existing keychain writer (`mur-core/src/bridge_keychain.rs`) for `keychain_set`; reuse the model registry's secret-ref pattern. `now_epoch()`/`oauth_redirect_port()` are small helpers (config + `SystemTime`).

- [ ] **Step 5: Wire `AgentMcpAction::Login { name, server }`** in cli/agent.rs + the async dispatch arm:

```rust
    AgentMcpAction::Login { name, server } =>
        cmd::agent::mcp_login::cmd_mcp_login(&name, &server).await?,
```

- [ ] **Step 6: Run the parse test + clippy**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime parses_token_response && ORT_STRATEGY=download cargo clippy -p mur-agent-runtime -p mur-core --no-deps -- -D warnings`
Expected: PASS + `Finished`.

- [ ] **Step 7: Commit**

```bash
git add mur-agent-runtime/src/oauth/mod.rs mur-core/src/cmd/agent/mcp_login.rs mur-core/src/cli/agent.rs mur-core/src/dispatch.rs
git commit -m "feat(oauth): auth-code+PKCE flow, localhost callback, token storage; mur agent mcp login"
```

## Task 10: Wire OAuth tokens into `HttpMcpClient` (resolve + refresh-on-401)

**Files:**
- Modify: `mur-agent-runtime/src/protocol/mcp_client.rs` (`resolve_bearer` → handle `Oauth`)
- Modify: `mur-agent-runtime/src/protocol/http_mcp_client.rs` (refresh on `Unauthorized`)

**Interfaces:**
- Consumes: `OauthAuth`, `oauth::refresh` (Task 9).

- [ ] **Step 1: Extend `resolve_bearer`** to resolve the OAuth access token

```rust
async fn resolve_bearer(entry: &McpServerEntry) -> Result<Option<String>, McpError> {
    match &entry.auth {
        Some(McpAuth::Bearer { token }) =>
            Ok(Some(token.resolve().map_err(|e| McpError::Transport(e.to_string()))?)),
        Some(McpAuth::Oauth(o)) =>
            Ok(Some(o.access_token.resolve().map_err(|e| McpError::Transport(e.to_string()))?)),
        None => Ok(None),
    }
}
```

- [ ] **Step 2: Refresh on 401** — give `HttpMcpClient` the refresh material and retry once

When `entry.auth` is `Oauth`, pass `Some(RefreshCtx { token_endpoint, client_id, refresh_token: o.refresh_token.resolve()? })` into `HttpMcpClient::connect`. In `request`, on `Err(McpError::Unauthorized)` with a `RefreshCtx`, call `oauth::refresh(...)`, swap in the new access token (`self.bearer`), persist it back to the keychain ref, and retry the request once. (Add a `refresh: Option<RefreshCtx>` field + a single-retry guard.)

```rust
// in request(), after getting UNAUTHORIZED:
if let Some(ctx) = &self.refresh && !retried {
    let t = crate::oauth::refresh(&self.http, &ctx.token_endpoint, &ctx.client_id, &ctx.refresh_token).await
        .map_err(|e| McpError::Transport(e.to_string()))?;
    *self.bearer_mut() = Some(t.access_token.clone());
    ctx.persist(&t.access_token); // write back to keychain
    return self.request_inner(method, params, /*retried=*/true).await;
}
```

> Split `request` into `request` (entry) + `request_inner(method, params, retried)` so the retry doesn't recurse forever. `// ponytail: one retry, then surface the 401.`

- [ ] **Step 3: Build + clippy**

Run: `ORT_STRATEGY=download cargo clippy -p mur-agent-runtime --no-deps -- -D warnings`
Expected: `Finished`.

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/protocol/mcp_client.rs mur-agent-runtime/src/protocol/http_mcp_client.rs
git commit -m "feat(oauth): resolve OAuth access token + refresh-on-401 retry in HttpMcpClient"
```

## Task 11: Live OAuth verify + docs

- [ ] **Step 1: Live verify** against a real OAuth-protected MCP server (e.g. a hosted GitHub/Linear MCP, or a local server with OAuth enabled)

```bash
cargo run -q --bin mur -- agent mcp add-remote me gh https://api.githubcopilot.com/mcp/
cargo run -q --bin mur -- agent mcp login me gh          # browser opens; authorize
# start the runtime; confirm tools/list loads; let a token expire; confirm refresh
```

Expected: `login` completes via the browser; the agent lists + calls the remote server's tools; a 401 after expiry triggers a silent refresh.

- [ ] **Step 2: Docs** — update `docs/architecture/runtime-overview.md` (remote MCP section) + the `mur agent ... mcp` surface line in `CLAUDE.md` to mention `add-remote` / `login` / registry remote install.

- [ ] **Step 3: Commit + PR**

```bash
git add docs/ CLAUDE.md
git commit -m "docs(mcp): remote MCP (Streamable HTTP + OAuth) usage"
```

PR it as "Remote MCP — OAuth 2.1 (discovery + DCR + PKCE + refresh)".

---

## Self-Review

**Spec coverage** (the user's ask: "remote MCP `type:http` + OAuth 2.1"):
- Streamable HTTP transport → Tasks 2–4 (`HttpMcpClient`, SSE parse, enum dispatch). ✅
- `type:http` server entries → Task 1 schema (`url`) + Task 5/6 CLI/registry install. ✅
- OAuth 2.1: discovery (RFC 9728/8414) Task 8; dynamic client registration (RFC 7591) Task 8; auth-code+PKCE (RFC 7636) Tasks 7+9; `resource` (RFC 8707) Task 9; refresh Task 10. ✅
- Token storage → Keychain `SecretRef` (Task 9). ✅
- Sandbox interplay: a remote MCP makes outbound HTTPS from the runtime — the per-server egress allowlist (PR #508) governs it; **note for the implementer:** add the remote host to the entry's `network.allow` so the runtime's egress proxy permits it (cross-reference `docs/superpowers/plans/2026-06-26-mcp-per-server-egress.md`). Gap closed by documenting, not code.

**Placeholder scan:** no TBD/TODO; every code step has complete code. Two `// ponytail:` ceiling notes (full-body SSE read; single refresh retry) are deliberate documented simplifications, not placeholders.

**Type consistency:** `McpClient` (enum) / `StdioMcpClient` / `HttpMcpClient`; `McpAuth::{Bearer,Oauth}` + `OauthAuth`; `AsMetadata`, `Tokens`; `cmd_mcp_add_remote`, `cmd_mcp_login`; `parse_sse_events`/`jsonrpc_result_for`/`parse_as_metadata`/`parse_tokens`/`code_verifier`/`code_challenge` — names consistent across tasks. `McpError` gains `Transport`/`Rpc`/`Protocol`/`Unauthorized` (Task 3) used through Task 10.

**Phasing:** Phase 1 (Tasks 1–6) ships independently as bearer-token remote MCP; Phase 2 (Tasks 7–11) adds OAuth. Either phase is a self-contained PR.
