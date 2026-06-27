# Feather P1 — Remote MCP by URL (Hub) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user add a *remote* (Streamable-HTTP) MCP server to an agent from the MUR Hub by entering its URL, with a pre-save connection test, a consent screen that shows the server's full tool descriptions, bearer-token or OAuth auth, and an egress allowlist defaulted to the server's own host.

**Architecture:** A new one-shot remote probe in `mur-core` (`initialize` + `tools/list`, 401/auth detection) is shared by CLI and Hub. Three thin Tauri commands in the Hub backend wrap the probe, the existing `cmd_mcp_add_remote`, and the existing OAuth `cmd_mcp_login`. A new React modal (`McpAddRemoteModal`) drives the test → consent → save flow, wired into `DetailPanel` next to the existing Discover button. No new profile fields — `McpServerEntry` already has `url`, `auth`, `description_hash`, `network`, `publisher`.

**Tech Stack:** Rust (mur-core, `reqwest` async, `serde_json`, `sha2`), Tauri 2 commands, React + TypeScript (Vite), existing i18n (`en.ts`/`zh-TW.ts`).

## Global Constraints

- Brand name user-facing is uppercase **MUR**; internal slugs lowercase. (CLAUDE.md rule 7)
- No hardcoded values — use constants/config. (CLAUDE.md rule 1)
- Single source file ≤ 800 lines; split siblings if approaching. (CLAUDE.md rule 4)
- Rust edition 2024 (`let`-chains allowed).
- Tests run under `cargo nextest` / plain `cargo test`; build/test mur-core with `ORT_STRATEGY=download`. Use the toolchain cargo at `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin` if the rustup proxy is broken.
- Hub UI typecheck: `npx tsc --noEmit`; build: `npm run build` (both run from `mur-hub-gui/ui`).
- Hub Rust fmt must be run with its own manifest: `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml`.
- Secrets (tokens) go to the OS keychain via `cmd_secret_set` and are referenced by `SecretRef::Keychain`; never write a raw token into `profile.yaml` or logs.
- Remote URLs must be `https://` (sole exception: host `localhost`/`127.0.0.1`).
- Profile changes apply on agent **restart** — surface this in UI copy.

---

## File Structure

- **Create** `mur-core/src/cmd/agent/mcp_remote.rs` — URL validation + the remote probe (`initialize`/`tools/list`/401) and pure parse helpers. Registered as a submodule of `cmd/agent`.
- **Modify** `mur-core/src/cmd/agent/mod.rs` — add `pub mod mcp_remote;`.
- **Modify** `mur-core/src/cmd/agent/mcp.rs` — extend `cmd_mcp_add_remote` to also accept a pinned `description_hash` and a default network host.
- **Modify** `mur-core/src/cmd/agent/cli/mod.rs` (or wherever the CLI calls `cmd_mcp_add_remote`) — update the call site for the new signature.
- **Modify** `mur-hub-gui/src-tauri/src/mcp_skills.rs` — three new Tauri commands.
- **Modify** `mur-hub-gui/src-tauri/src/lib.rs` — register the three commands in `generate_handler!`.
- **Create** `mur-hub-gui/ui/src/components/McpAddRemoteModal.tsx` — the modal.
- **Modify** `mur-hub-gui/ui/src/components/DetailPanel.tsx` — add the "Add by URL" button + modal wiring.
- **Modify** `mur-hub-gui/ui/src/i18n/en.ts` and `zh-TW.ts` — new strings.
- **Modify** `mur-hub-gui/ui/src/styles/components/modal.css` — only if a new sub-element needs a rule (reuse `.input`, `.modal__*` from the search/scroll work).

---

## Task 1: URL validation (mur-core)

**Files:**
- Create: `mur-core/src/cmd/agent/mcp_remote.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs` (add `pub mod mcp_remote;`)
- Test: inline `#[cfg(test)]` in `mcp_remote.rs`

**Interfaces:**
- Produces: `pub fn validate_remote_url(raw: &str) -> anyhow::Result<String>` — trims, parses, enforces https (localhost http allowed), returns the normalized URL string.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_https_and_rejects_http_except_localhost() {
        assert_eq!(
            validate_remote_url(" https://mcp.example.com/mcp ").unwrap(),
            "https://mcp.example.com/mcp"
        );
        assert!(validate_remote_url("http://mcp.example.com/mcp").is_err());
        assert!(validate_remote_url("http://localhost:8080/mcp").is_ok());
        assert!(validate_remote_url("http://127.0.0.1:8080/mcp").is_ok());
        assert!(validate_remote_url("not a url").is_err());
        assert!(validate_remote_url("ftp://x/y").is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core mcp_remote::tests::accepts_https -- --nocapture`
Expected: FAIL — `validate_remote_url` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! `mur agent mcp` remote-server probe: one-shot `initialize` + `tools/list`
//! handshake used to validate and preview a remote (Streamable-HTTP) MCP server
//! before it is added. Shared by the CLI and the MUR Hub. Network-free helpers
//! are split out so transport/auth detection is unit-testable.

use anyhow::{Result, bail};
use reqwest::Url;

/// Validate and normalize a remote MCP URL. Requires `https`, except `http` on
/// `localhost`/`127.0.0.1` for local development.
pub fn validate_remote_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let url = Url::parse(trimmed).map_err(|e| anyhow::anyhow!("invalid URL: {e}"))?;
    let host = url.host_str().unwrap_or("");
    let is_local = matches!(host, "localhost" | "127.0.0.1" | "::1");
    match url.scheme() {
        "https" => {}
        "http" if is_local => {}
        "http" => bail!("remote MCP URLs must use https (got http://{host})"),
        other => bail!("unsupported URL scheme: {other}"),
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}
```

Also add to `mur-core/src/cmd/agent/mod.rs`:

```rust
pub mod mcp_remote;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core mcp_remote::tests::accepts_https`
Expected: PASS.

Note: `validate_remote_url` strips a single trailing `/`; the test URLs have paths so they are unaffected. (A bare `https://host/` normalizes to `https://host`.)

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/mcp_remote.rs mur-core/src/cmd/agent/mod.rs
git commit -m "feat(mcp): validate_remote_url for remote MCP add"
```

---

## Task 2: Parse helpers — 401 metadata + tools/list (mur-core, network-free)

**Files:**
- Modify: `mur-core/src/cmd/agent/mcp_remote.rs`
- Test: inline tests

**Interfaces:**
- Produces:
  - `pub struct ProbeTool { pub name: String, pub description: String, pub input_schema: serde_json::Value }`
  - `pub fn parse_resource_metadata(www_authenticate: &str) -> Option<String>` — extract `resource_metadata="..."`.
  - `pub fn parse_tools_list(body: &serde_json::Value) -> Vec<ProbeTool>` — read JSON-RPC `result.tools[]`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn parses_resource_metadata_param() {
    let h = r#"Bearer error="invalid_token", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource""#;
    assert_eq!(
        parse_resource_metadata(h).as_deref(),
        Some("https://mcp.example.com/.well-known/oauth-protected-resource")
    );
    assert_eq!(parse_resource_metadata("Bearer").none_is(), None.none_is());
}

#[test]
fn parses_tools_from_jsonrpc_result() {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 2,
        "result": { "tools": [
            { "name": "search", "description": "Search docs",
              "inputSchema": { "type": "object" } }
        ]}
    });
    let tools = parse_tools_list(&body);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "search");
    assert_eq!(tools[0].description, "Search docs");
}
```

(Replace the `.none_is()` placeholder line with the real assertion below in Step 3's note — written here only to keep the example compiling-shaped; use `assert!(parse_resource_metadata("Bearer").is_none());`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core mcp_remote::tests::parses_`
Expected: FAIL — functions/types not found.

- [ ] **Step 3: Write minimal implementation**

Add to `mcp_remote.rs`:

```rust
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProbeTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Pull the `resource_metadata="..."` URL out of a `WWW-Authenticate` header
/// (RFC 9728). Returns `None` if absent.
pub fn parse_resource_metadata(www_authenticate: &str) -> Option<String> {
    let key = "resource_metadata=";
    let start = www_authenticate.find(key)? + key.len();
    let rest = &www_authenticate[start..];
    let rest = rest.trim_start_matches('"');
    let end = rest.find('"').unwrap_or(rest.len());
    let val = rest[..end].trim();
    (!val.is_empty()).then(|| val.to_string())
}

/// Extract tools from a JSON-RPC `tools/list` response body. Tolerant: missing
/// fields default to empty. Accepts `inputSchema` (spec) or `input_schema`.
pub fn parse_tools_list(body: &serde_json::Value) -> Vec<ProbeTool> {
    let Some(tools) = body.get("result").and_then(|r| r.get("tools")).and_then(|t| t.as_array())
    else {
        return Vec::new();
    };
    tools
        .iter()
        .map(|t| ProbeTool {
            name: t.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            description: t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            input_schema: t
                .get("inputSchema")
                .or_else(|| t.get("input_schema"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        })
        .collect()
}
```

Fix the test's last line of `parses_resource_metadata_param` to: `assert!(parse_resource_metadata("Bearer").is_none());`

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core mcp_remote::tests::parses_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/mcp_remote.rs
git commit -m "feat(mcp): parse 401 resource_metadata + tools/list (network-free)"
```

---

## Task 3: The async probe (mur-core)

**Files:**
- Modify: `mur-core/src/cmd/agent/mcp_remote.rs`
- Test: inline test for the request-body builder (network call itself is covered by manual/live testing)

**Interfaces:**
- Produces:
  - `pub enum ProbeTransport { StreamableHttp, LegacySse, Unknown }` (Serialize, lowercase via serde rename)
  - `pub struct ProbeOutcome { pub transport: ProbeTransport, pub needs_auth: bool, pub resource_metadata: Option<String>, pub tools: Vec<ProbeTool> }` (Serialize)
  - `pub fn initialize_request_body() -> serde_json::Value`
  - `pub async fn probe_remote(url: &str, bearer: Option<&str>) -> anyhow::Result<ProbeOutcome>`
- Consumes: Task 1 `validate_remote_url`, Task 2 `parse_resource_metadata`/`parse_tools_list`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn initialize_body_has_protocol_and_client() {
    let b = initialize_request_body();
    assert_eq!(b["method"], "initialize");
    assert_eq!(b["params"]["protocolVersion"], MCP_PROTOCOL_VERSION);
    assert_eq!(b["params"]["clientInfo"]["name"], "mur-hub");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core mcp_remote::tests::initialize_body`
Expected: FAIL — `initialize_request_body`/`MCP_PROTOCOL_VERSION` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `mcp_remote.rs`:

```rust
/// MCP protocol revision we advertise in `initialize`. Bump when MUR adopts a
/// newer spec. (Constant — not a hardcoded literal scattered in code.)
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeTransport {
    StreamableHttp,
    LegacySse,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeOutcome {
    pub transport: ProbeTransport,
    pub needs_auth: bool,
    pub resource_metadata: Option<String>,
    pub tools: Vec<ProbeTool>,
}

/// JSON-RPC `initialize` request advertised by MUR as the client.
pub fn initialize_request_body() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "mur-hub", "version": env!("CARGO_PKG_VERSION") }
        }
    })
}

fn tools_list_body() -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} })
}

/// One-shot probe of a remote MCP server. POSTs `initialize`; on `401` reports
/// `needs_auth` + the RFC-9728 metadata URL; on success reports Streamable-HTTP
/// and fetches `tools/list`; on 400/404/405 reports the deprecated SSE transport.
pub async fn probe_remote(url: &str, bearer: Option<&str>) -> Result<ProbeOutcome> {
    let url = validate_remote_url(url)?;
    let http = reqwest::Client::new();

    let mut req = http
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&initialize_request_body());
    if let Some(tok) = bearer {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| anyhow::anyhow!("connect failed: {e}"))?;
    let status = resp.status();

    if status.as_u16() == 401 {
        let wa = resp
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        return Ok(ProbeOutcome {
            transport: ProbeTransport::StreamableHttp,
            needs_auth: true,
            resource_metadata: parse_resource_metadata(wa),
            tools: Vec::new(),
        });
    }

    if matches!(status.as_u16(), 400 | 404 | 405) {
        return Ok(ProbeOutcome {
            transport: ProbeTransport::LegacySse,
            needs_auth: false,
            resource_metadata: None,
            tools: Vec::new(),
        });
    }

    if !status.is_success() {
        bail!("server returned {status}");
    }

    // tools/list (best-effort; a server may require a session we don't keep —
    // an empty list still lets the user add it, just without a preview).
    let mut tools = Vec::new();
    let mut tl = http
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&tools_list_body());
    if let Some(tok) = bearer {
        tl = tl.bearer_auth(tok);
    }
    if let Ok(r) = tl.send().await
        && r.status().is_success()
        && let Ok(body) = r.json::<serde_json::Value>().await
    {
        tools = parse_tools_list(&body);
    }

    Ok(ProbeOutcome {
        transport: ProbeTransport::StreamableHttp,
        needs_auth: false,
        resource_metadata: None,
        tools,
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core mcp_remote::tests::initialize_body`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/mcp_remote.rs
git commit -m "feat(mcp): probe_remote initialize/tools-list handshake"
```

---

## Task 4: Extend `cmd_mcp_add_remote` with hash pin + default egress (mur-core)

**Files:**
- Modify: `mur-core/src/cmd/agent/mcp.rs:255` (`cmd_mcp_add_remote`)
- Modify: CLI call site (search `cmd_mcp_add_remote(` outside `mcp.rs`/`mcp_registry.rs`)
- Test: inline test in `mcp.rs`

**Interfaces:**
- Produces (new signature): `pub fn cmd_mcp_add_remote(agent: &str, name: &str, url: &str, bearer: Option<SecretRef>, description_hash: Option<String>, egress_host: Option<&str>) -> anyhow::Result<()>`
- Consumes: existing `McpServerEntry`, `McpAuth`, `McpServerNetwork { mode, allow_hosts }`, `McpNetMode::Restricted`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn add_remote_sets_hash_and_default_egress() {
    // Build into a temp profile via the existing test harness used by
    // add_remote_writes_url_and_bearer (mirror its setup).
    // Assert the written entry has:
    //   entry.description_hash == Some("abc123".into())
    //   entry.network == Some(Restricted { allow_hosts: ["mcp.example.com"] })
    // (See add_remote_writes_url_and_bearer in this file for the harness.)
}
```

Fill the body by copying the harness from the existing `add_remote_writes_url_and_bearer` test (same file), then call `cmd_mcp_add_remote(agent, "srv", "https://mcp.example.com/mcp", None, Some("abc123".into()), Some("mcp.example.com"))` and assert the two fields.

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core agent::mcp::tests::add_remote_sets_hash`
Expected: FAIL — arity mismatch / fields not set.

- [ ] **Step 3: Write minimal implementation**

Change `cmd_mcp_add_remote` in `mcp.rs` to:

```rust
pub fn cmd_mcp_add_remote(
    agent: &str,
    name: &str,
    url: &str,
    bearer: Option<mur_common::secret::SecretRef>,
    description_hash: Option<String>,
    egress_host: Option<&str>,
) -> anyhow::Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    if profile.mcp_servers.iter().any(|m| m.name == name) {
        anyhow::bail!("MCP server '{name}' already exists on '{agent}'; remove it first");
    }
    let network = egress_host.map(|h| mur_common::agent::McpServerNetwork {
        mode: mur_common::agent::McpNetMode::Restricted,
        allow_hosts: vec![h.to_string()],
    });
    profile.mcp_servers.push(mur_common::agent::McpServerEntry {
        name: name.to_string(),
        url: Some(url.to_string()),
        auth: bearer.map(|token| mur_common::agent::McpAuth::Bearer { token }),
        description_hash,
        network,
        installed_at: Some(chrono::Utc::now()),
        ..Default::default()
    });
    save_profile(&path, &mut profile)?;
    println!("Added remote MCP server '{name}' → {url} for agent '{agent}'.");
    Ok(())
}
```

Update the existing CLI call site to pass `None, None` for the two new params (preserving today's behavior). Update the existing `add_remote_writes_url_and_bearer` test call to the new arity (`..., None, None`).

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core agent::mcp::tests::add_remote`
Expected: PASS (both add_remote tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/mcp.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(mcp): cmd_mcp_add_remote pins tool-schema hash + default egress host"
```

---

## Task 5: Tauri `agent_mcp_test_connection` (Hub backend)

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/mcp_skills.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs:531` (`generate_handler!`)

**Interfaces:**
- Produces: `#[tauri::command] pub async fn agent_mcp_test_connection(url: String, bearer: Option<String>) -> Result<mur_core::cmd::agent::mcp_remote::ProbeOutcome, String>`
- Consumes: Task 3 `probe_remote`.

- [ ] **Step 1: Add the command**

```rust
use mur_core::cmd::agent::mcp_remote::{ProbeOutcome, probe_remote};

/// Probe a remote MCP URL before adding it: reports transport, whether auth is
/// required, and the tool list for the consent screen. Read-only; writes nothing.
#[tauri::command]
pub async fn agent_mcp_test_connection(
    url: String,
    bearer: Option<String>,
) -> Result<ProbeOutcome, String> {
    probe_remote(&url, bearer.as_deref())
        .await
        .map_err(|e| format!("{e:#}"))
}
```

- [ ] **Step 2: Register it** in `lib.rs` `generate_handler!` (alongside `mcp_skills::mcp_discover,`):

```rust
            mcp_skills::agent_mcp_test_connection,
```

- [ ] **Step 3: Verify it compiles**

Run: `ORT_STRATEGY=download cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: compiles (warnings ok). Note: needs `mur-hub-gui/ui/dist` present for `generate_context!` — build the UI once first (`cd mur-hub-gui/ui && npm run build`) or keep the existing dist.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/src/mcp_skills.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): agent_mcp_test_connection Tauri command"
```

---

## Task 6: Tauri `agent_mcp_add_remote` (Hub backend)

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/mcp_skills.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (`generate_handler!`)

**Interfaces:**
- Produces: `#[tauri::command] pub async fn agent_mcp_add_remote(name: String, server_id: String, url: String, bearer: Option<String>) -> Result<AgentDetail, String>`
- Consumes: Task 3 `probe_remote`, Task 4 `cmd_mcp_add_remote`, existing `cmd_secret_set`, `compute_description_hash`.

- [ ] **Step 1: Add the command**

```rust
use mur_core::cmd::agent::agent_mcp_pin::{McpToolDescription, compute_description_hash};
use mur_core::cmd::agent::mcp::cmd_mcp_add_remote;
use mur_core::cmd::agent::mcp_remote::validate_remote_url;
use mur_core::cmd::agent::secret::cmd_secret_set;
use mur_common::secret::SecretRef;

const MCP_SECRET_SERVICE: &str = "run.mur.agent"; // matches secret.rs SECRET_SERVICE

/// Add a remote (Streamable-HTTP) MCP server by URL. Stores a bearer token (if
/// any) in the OS keychain and references it; pins a hash of the server's tool
/// schemas; defaults egress to the server's host.
#[tauri::command]
pub async fn agent_mcp_add_remote(
    name: String,
    server_id: String,
    url: String,
    bearer: Option<String>,
) -> Result<AgentDetail, String> {
    let url = validate_remote_url(&url).map_err(|e| format!("{e:#}"))?;
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));

    // Probe to capture the tool-schema hash for the pin (best-effort).
    let outcome = probe_remote(&url, bearer.as_deref()).await.map_err(|e| format!("{e:#}"))?;
    let desc_hash = if outcome.tools.is_empty() {
        None
    } else {
        let tools: Vec<McpToolDescription> = outcome
            .tools
            .iter()
            .map(|t| McpToolDescription {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect();
        Some(compute_description_hash(&tools))
    };

    // Store bearer in keychain → SecretRef.
    let secret_ref = if let Some(tok) = bearer.as_deref().filter(|s| !s.is_empty()) {
        let key = format!("mcp/{server_id}/bearer");
        cmd_secret_set(&name, &key, Some(tok)).await.map_err(|e| format!("{e:#}"))?;
        let account = format!("{name}/{key}");
        Some(SecretRef::Keychain { service: MCP_SECRET_SERVICE.to_string(), account })
    } else {
        None
    };

    cmd_mcp_add_remote(&name, &server_id, &url, secret_ref, desc_hash, host.as_deref())
        .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}
```

> **Implementer note:** confirm `SECRET_SERVICE` and the account-derivation (`acct`) in `mur-core/src/cmd/agent/secret.rs:9` and mirror them exactly so the runtime resolves the same keychain item. If `cmd_secret_set` already derives the account, expose/return it rather than re-deriving here. `McpToolDescription`'s fields are defined in `mur-core/src/cmd/agent/agent_mcp_pin.rs` (name/description/input_schema) — match them.

- [ ] **Step 2: Register** in `lib.rs`: `mcp_skills::agent_mcp_add_remote,`

- [ ] **Step 3: Verify it compiles**

Run: `ORT_STRATEGY=download cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: compiles. Fix any field/name mismatches flagged by the implementer note.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/src/mcp_skills.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): agent_mcp_add_remote (keychain bearer + schema pin + egress)"
```

---

## Task 7: Tauri `agent_mcp_oauth_login` (Hub backend)

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/mcp_skills.rs`, `lib.rs`

**Interfaces:**
- Produces: `#[tauri::command] pub async fn agent_mcp_oauth_login(name: String, server_id: String, url: String) -> Result<AgentDetail, String>`
- Consumes: Task 4 `cmd_mcp_add_remote` (to create the url-only entry first), existing `cmd_mcp_login`.

- [ ] **Step 1: Add the command**

```rust
use mur_core::cmd::agent::mcp_login::cmd_mcp_login;

/// Add a remote MCP server entry (url only) then run the OAuth 2.1 + PKCE login
/// flow against it (opens the system browser). The token is stored by the login
/// flow; the entry's auth becomes OAuth.
#[tauri::command]
pub async fn agent_mcp_oauth_login(
    name: String,
    server_id: String,
    url: String,
) -> Result<AgentDetail, String> {
    let url = validate_remote_url(&url).map_err(|e| format!("{e:#}"))?;
    let host = reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string));
    // Create the entry first (cmd_mcp_login looks it up by name to read the url).
    cmd_mcp_add_remote(&name, &server_id, &url, None, None, host.as_deref())
        .map_err(|e| format!("{e:#}"))?;
    cmd_mcp_login(&name, &server_id).await.map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}
```

- [ ] **Step 2: Register** in `lib.rs`: `mcp_skills::agent_mcp_oauth_login,`

- [ ] **Step 3: Verify it compiles**

Run: `ORT_STRATEGY=download cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/src/mcp_skills.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): agent_mcp_oauth_login Tauri command"
```

---

## Task 8: i18n strings

**Files:**
- Modify: `mur-hub-gui/ui/src/i18n/en.ts`, `mur-hub-gui/ui/src/i18n/zh-TW.ts`

**Interfaces:**
- Produces keys: `detail.addRemoteMcp`, `remote.title`, `remote.url`, `remote.urlPlaceholder`, `remote.auth`, `remote.authNone`, `remote.authBearer`, `remote.authOauth`, `remote.token`, `remote.test`, `remote.testing`, `remote.testOk`, `remote.needsAuth`, `remote.legacyWarn`, `remote.toolsHeading`, `remote.noTools`, `remote.add`, `remote.adding`, `remote.oauthLogin`, `remote.restartHint`, `remote.invalidUrl`.

- [ ] **Step 1: Add to `en.ts`** (after the `detail.discover*` block):

```ts
  "detail.addRemoteMcp": "Add by URL",
  "remote.title": "Add remote MCP server",
  "remote.url": "Server URL",
  "remote.urlPlaceholder": "https://mcp.example.com/mcp",
  "remote.auth": "Authentication",
  "remote.authNone": "None",
  "remote.authBearer": "Bearer token",
  "remote.authOauth": "OAuth",
  "remote.token": "Bearer token",
  "remote.test": "Test connection",
  "remote.testing": "Testing…",
  "remote.testOk": "Connected ✓",
  "remote.needsAuth": "This server requires authentication. Use OAuth or a bearer token.",
  "remote.legacyWarn": "This server uses the deprecated SSE transport (sunsetting 2026-06-30).",
  "remote.toolsHeading": "Tools this server exposes (review before adding)",
  "remote.noTools": "No tools previewed (the server may require an authenticated session).",
  "remote.add": "Add server",
  "remote.adding": "Adding…",
  "remote.oauthLogin": "Sign in with OAuth",
  "remote.restartHint": "Added. Restart the agent to load it.",
  "remote.invalidUrl": "Enter an https:// URL (http allowed only for localhost).",
```

- [ ] **Step 2: Add the same keys to `zh-TW.ts`** with translations:

```ts
  "detail.addRemoteMcp": "用網址新增",
  "remote.title": "新增遠端 MCP 伺服器",
  "remote.url": "伺服器網址",
  "remote.urlPlaceholder": "https://mcp.example.com/mcp",
  "remote.auth": "驗證方式",
  "remote.authNone": "無",
  "remote.authBearer": "Bearer 權杖",
  "remote.authOauth": "OAuth",
  "remote.token": "Bearer 權杖",
  "remote.test": "測試連線",
  "remote.testing": "測試中…",
  "remote.testOk": "已連線 ✓",
  "remote.needsAuth": "此伺服器需要驗證，請使用 OAuth 或 Bearer 權杖。",
  "remote.legacyWarn": "此伺服器使用已淘汰的 SSE 傳輸（將於 2026-06-30 停用）。",
  "remote.toolsHeading": "此伺服器提供的工具（新增前請先檢視）",
  "remote.noTools": "無法預覽工具（伺服器可能需要已驗證的連線）。",
  "remote.add": "新增伺服器",
  "remote.adding": "新增中…",
  "remote.oauthLogin": "使用 OAuth 登入",
  "remote.restartHint": "已新增。重新啟動 agent 後生效。",
  "remote.invalidUrl": "請輸入 https:// 網址（http 僅限 localhost）。",
```

- [ ] **Step 3: Typecheck**

Run: `cd mur-hub-gui/ui && npx tsc --noEmit`
Expected: exit 0 (the `Table` type requires both locales to have matching keys).

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "i18n(hub): remote MCP add strings (en + zh-TW)"
```

---

## Task 9: `McpAddRemoteModal` component

**Files:**
- Create: `mur-hub-gui/ui/src/components/McpAddRemoteModal.tsx`

**Interfaces:**
- Produces: `export function McpAddRemoteModal({ agentName, onClose, onSaved }: Props)` where `Props = { agentName: string; onClose: () => void; onSaved: (d: AgentDetail) => void }`.
- Consumes: Tauri `agent_mcp_test_connection`, `agent_mcp_add_remote`, `agent_mcp_oauth_login`; reuses `.modal*`/`.input`/`.item-list`/`.item-card` CSS.

- [ ] **Step 1: Write the component**

```tsx
//! Add a remote (Streamable-HTTP) MCP server by URL: validate → test
//! connection → review the server's full tool descriptions → add. Bearer tokens
//! go to the keychain (backend); OAuth opens the system browser.
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";
import type { AgentDetail } from "../types";

type Auth = "none" | "bearer" | "oauth";
interface ProbeTool { name: string; description: string; input_schema: unknown }
interface ProbeOutcome {
  transport: "streamable_http" | "legacy_sse" | "unknown";
  needs_auth: boolean;
  resource_metadata: string | null;
  tools: ProbeTool[];
}
interface Props { agentName: string; onClose: () => void; onSaved: (d: AgentDetail) => void }

export function McpAddRemoteModal({ agentName, onClose, onSaved }: Props) {
  const { t } = useT();
  const [url, setUrl] = useState("");
  const [serverId, setServerId] = useState("");
  const [auth, setAuth] = useState<Auth>("none");
  const [token, setToken] = useState("");
  const [probe, setProbe] = useState<ProbeOutcome | null>(null);
  const [busy, setBusy] = useState<null | "test" | "add">(null);
  const [error, setError] = useState<string | null>(null);

  async function test() {
    setError(null); setBusy("test"); setProbe(null);
    try {
      const out = await invoke<ProbeOutcome>("agent_mcp_test_connection", {
        url, bearer: auth === "bearer" && token ? token : null,
      });
      setProbe(out);
      if (out.needs_auth && auth === "none") setAuth("oauth");
    } catch (e) { setError(String(e)); } finally { setBusy(null); }
  }

  async function add() {
    setError(null); setBusy("add");
    try {
      const id = serverId.trim() || hostOf(url);
      let detail: AgentDetail;
      if (auth === "oauth") {
        detail = await invoke<AgentDetail>("agent_mcp_oauth_login", { name: agentName, serverId: id, url });
      } else {
        detail = await invoke<AgentDetail>("agent_mcp_add_remote", {
          name: agentName, serverId: id, url, bearer: auth === "bearer" && token ? token : null,
        });
      }
      onSaved(detail);
      onClose();
    } catch (e) { setError(String(e)); } finally { setBusy(null); }
  }

  return (
    <div className="modal__overlay" onClick={onClose}>
      <div className="modal modal--wide" onClick={(e) => e.stopPropagation()}>
        <div className="modal__header">
          <h2 className="modal__title">{t("remote.title")}</h2>
          <button className="modal__close" onClick={onClose} aria-label={t("detail.close")}>×</button>
        </div>
        <div className="modal__body">
          <label className="field-muted">{t("remote.url")}</label>
          <input className="input" type="url" placeholder={t("remote.urlPlaceholder")}
                 value={url} onChange={(e) => { setUrl(e.target.value); setProbe(null); }} autoFocus />

          <div style={{ marginTop: 10 }}>
            <label className="field-muted">{t("remote.auth")}</label>
            <div className="mcp-form-actions">
              {(["none", "bearer", "oauth"] as Auth[]).map((a) => (
                <label key={a} style={{ marginRight: 12 }}>
                  <input type="radio" name="auth" checked={auth === a} onChange={() => setAuth(a)} />{" "}
                  {t(`remote.auth${a[0].toUpperCase()}${a.slice(1)}` as Parameters<typeof t>[0])}
                </label>
              ))}
            </div>
          </div>

          {auth === "bearer" && (
            <input className="input" type="password" placeholder={t("remote.token")}
                   value={token} onChange={(e) => setToken(e.target.value)} style={{ marginTop: 8 }} />
          )}

          <div className="mcp-form-actions" style={{ marginTop: 12 }}>
            <button className="btn btn--sm btn--secondary" disabled={!url || busy !== null} onClick={test}>
              {busy === "test" ? t("remote.testing") : t("remote.test")}
            </button>
          </div>

          {probe && (
            <div style={{ marginTop: 12 }}>
              {probe.transport === "legacy_sse" && <p className="field-muted">{t("remote.legacyWarn")}</p>}
              {probe.needs_auth && <p className="field-muted">{t("remote.needsAuth")}</p>}
              {!probe.needs_auth && <p className="field-muted">{t("remote.testOk")}</p>}
              <p className="field-muted" style={{ marginTop: 8 }}>{t("remote.toolsHeading")}</p>
              {probe.tools.length === 0 ? (
                <p className="field-muted">{t("remote.noTools")}</p>
              ) : (
                <ul className="item-list">
                  {probe.tools.map((tool) => (
                    <li key={tool.name} className="item-card">
                      <div className="item-card-name">{tool.name}</div>
                      <code className="item-card-code">{tool.description || "—"}</code>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}

          {error && <p className="save-error">{error}</p>}
          <p className="field-muted" style={{ marginTop: 8 }}>{t("remote.restartHint")}</p>
        </div>
        <div className="modal__footer">
          <button className="btn btn--sm btn--secondary" onClick={onClose}>{t("detail.close")}</button>
          <button className="btn btn--sm btn--primary"
                  disabled={!probe || busy !== null}
                  onClick={add}>
            {busy === "add" ? t("remote.adding") : auth === "oauth" ? t("remote.oauthLogin") : t("remote.add")}
          </button>
        </div>
      </div>
    </div>
  );
}

function hostOf(u: string): string {
  try { return new URL(u).hostname.replace(/\./g, "-"); } catch { return "remote"; }
}
```

- [ ] **Step 2: Typecheck**

Run: `cd mur-hub-gui/ui && npx tsc --noEmit`
Expected: exit 0. (If the dynamic `t(\`remote.auth…\`)` key type errors, replace with an explicit map `{none: t("remote.authNone"), bearer: t("remote.authBearer"), oauth: t("remote.authOauth")}[a]`.)

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/ui/src/components/McpAddRemoteModal.tsx
git commit -m "feat(hub): McpAddRemoteModal (test → consent → add)"
```

---

## Task 10: Wire the modal into `DetailPanel` + live verify

**Files:**
- Modify: `mur-hub-gui/ui/src/components/DetailPanel.tsx` (near the Discover button ~line 1078-1090)

**Interfaces:**
- Consumes: Task 9 `McpAddRemoteModal`; existing `onSaved` handler (already passed to `McpDiscoverModal` as `onImported`).

- [ ] **Step 1: Import + state + button + render**

Add the import near the `McpDiscoverModal` import:

```tsx
import { McpAddRemoteModal } from "./McpAddRemoteModal";
```

Add state next to `showDiscover`:

```tsx
  const [showAddRemote, setShowAddRemote] = useState(false);
```

Add a button next to the `discoverMcp` button:

```tsx
          <button className="btn btn--sm btn--secondary" onClick={() => setShowAddRemote(true)}>
            {t("detail.addRemoteMcp")}
          </button>
```

Add the modal render next to the `McpDiscoverModal` render:

```tsx
      {showAddRemote && (
        <McpAddRemoteModal
          agentName={detail.name}
          onClose={() => setShowAddRemote(false)}
          onSaved={onSaved}
        />
      )}
```

(Use the same `agentName`/`onSaved` values the existing `McpDiscoverModal` uses — confirm the exact prop names at lines 1086-1089.)

- [ ] **Step 2: Typecheck + build**

Run: `cd mur-hub-gui/ui && npx tsc --noEmit && npm run build`
Expected: tsc exit 0; vite build succeeds.

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/ui/src/components/DetailPanel.tsx
git commit -m "feat(hub): wire Add-by-URL into the MCP tab"
```

- [ ] **Step 4: Live verify (manual)**

Rebuild + install the Hub (`docs/.../gotcha_hub_local_app_build_recipe`): stage sidecars, `npx @tauri-apps/cli@2 build --debug --bundles app` (native, GGML_NATIVE=OFF), ad-hoc sign, install, relaunch. Then in the Hub: open an agent → MCP tab → **Add by URL** → enter a known public remote MCP URL → **Test connection** → confirm the tool list renders with full descriptions → **Add** → confirm the server appears in the MCP list and (after agent restart) its tools work and HITL fires. For a bearer/OAuth server, confirm the token never appears in `profile.yaml`.

---

## Self-Review

**Spec coverage (P1 sections of the spec):**
- Validate URL (https/localhost) → Task 1. ✓
- Connection test before save (transport + 401 detection) → Tasks 3, 5. ✓
- OAuth (RFC 9728 discovery + PKCE) → reuses `cmd_mcp_login`, Task 7. ✓
- Bearer token → keychain `SecretRef` → Task 6. ✓
- Consent screen w/ full tool descriptions → Task 9. ✓
- Tool-schema hash pin (`description_hash`) → Tasks 4, 6. ✓
- Egress default = Restricted[host] → Task 4. ✓
- Hub entry point + wiring → Tasks 9, 10. ✓
- Legacy SSE detection + deprecation warning → Tasks 3, 9. ✓

**Placeholder scan:** Two intentional implementer-notes (Task 4 test harness reuse; Task 6 secret account-derivation + `McpToolDescription` field confirmation) point at exact existing locations to copy from rather than leaving logic undefined. The illustrative `.none_is()` line in Task 2 is explicitly corrected in the same task. No "TBD/handle errors/add validation" placeholders.

**Type consistency:** `ProbeOutcome`/`ProbeTool`/`ProbeTransport` defined in Task 3 are consumed verbatim in Tasks 5/6 and mirrored in the TS interface in Task 9 (`streamable_http`/`legacy_sse`/`unknown` match the serde `snake_case` rename). `cmd_mcp_add_remote` 6-arg signature defined in Task 4 is called with matching arity in Tasks 6 and 7 and the CLI site. `agent_mcp_add_remote`/`agent_mcp_test_connection`/`agent_mcp_oauth_login` names match between backend (Tasks 5-7) and `invoke(...)` calls (Task 9).

**Out of P1 scope (later phases):** registry/package install (P2); verified raw download into `~/.mur/mcp-servers` (P3); runtime-side re-verification of `description_hash` drift for remote servers (consent-time pin is stored in P1; enforcement is a follow-up).
