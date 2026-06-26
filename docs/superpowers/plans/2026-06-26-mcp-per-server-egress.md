# Per-MCP-Server Network Egress (Plan B — proxy, advisory) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user scope an individual MCP server's outbound network to an explicit host allowlist, enforced by routing that server's child process through a runtime-managed loopback egress proxy — without touching the agent's own LLM-proxy path (cc-proxy).

**Architecture:** Today network policy is per-AGENT only (`OutboundNetwork` on `AgentProfile`); MCP child processes either inherit a port-only OS sandbox (Linux) or run unconfined (macOS) and never pass through the agent's in-process host allowlist (`reqwest_guard`). We add an opt-in per-server policy (`McpServerEntry.network`). When present, the runtime starts a small loopback CONNECT proxy that enforces that server's host allowlist, and injects `HTTP_PROXY`/`HTTPS_PROXY` (with a per-server auth token) **only into that child's process environment**. A child with no policy is spawned byte-for-byte as today. **Enforcement is ADVISORY** (a cooperating tool honors `HTTP_PROXY`; a determined malicious tool can still connect direct because neither SBPL nor Landlock filters by host) — airtight containment needs Linux network namespaces + a macOS pre-fork launcher, both explicitly out of scope (see Task 7). The value delivered is per-server allowlisting + audit for trusted-but-scoped tools.

**Tech Stack:** Rust (mur-common types; mur-agent-runtime tokio runtime, reqwest LLM clients), existing deps only (`tokio`, `uuid`, `serde`) — no new crates.

## Global Constraints

- **Env isolation (hard rule).** `HTTP_PROXY`/`HTTPS_PROXY` are set ONLY on a policied child's `std::process::Command` env — NEVER on the runtime/agent process env. `NO_PROXY=127.0.0.1,localhost,::1` is set on those children so loopback (incl. the cc-proxy) is never double-proxied.
- **LLM client never inherits ambient proxy.** The three agent LLM reqwest clients are built with `.no_proxy()` so the agent's Anthropic/OpenAI/Ollama traffic is determined solely by their `base_url` (i.e. cc-proxy keeps working unchanged whether or not it is present).
- **Opt-in / zero-change default.** `McpServerEntry.network == None` ⇒ no proxy, no env injection ⇒ identical to today. The egress proxy is only started if at least one server on the agent has a `Restricted` policy.
- **Threat model is advisory, and it is documented as such** (Task 7). Do not claim containment of a malicious server.
- **No new dependencies.** Reuse `host_matches_pattern` (`mur-agent-runtime/src/sandbox/reqwest_guard.rs:13`); reuse `uuid` for the per-server token.
- Rust edition 2024 (let-chains stable). `mur-common` is types-only — no I/O there.

---

### Task 1: Per-server network schema

**Files:**
- Modify: `mur-common/src/agent.rs:242-281` (add field to `McpServerEntry`) and add the new types nearby.
- Test: inline `#[cfg(test)]` in `mur-common/src/agent.rs`.

**Interfaces:**
- Produces:
  - `pub enum McpNetMode { Inherit, Restricted, Off }` (serde `rename_all = "snake_case"`, default `Inherit`).
  - `pub struct McpServerNetwork { pub mode: McpNetMode, pub allow_hosts: Vec<String> }`.
  - `McpServerEntry.network: Option<McpServerNetwork>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn mcp_entry_network_is_optional_and_round_trips() {
    // Absent in YAML → None (every existing profile keeps working).
    let bare = "name: x\ncommand: npx\n";
    let e: McpServerEntry = serde_yaml_ng::from_str(bare).unwrap();
    assert!(e.network.is_none());

    // Present → parsed.
    let with = "name: browser\ncommand: npx\nnetwork:\n  mode: restricted\n  allow_hosts: [\"example.com\", \"*.api.example.com\"]\n";
    let e2: McpServerEntry = serde_yaml_ng::from_str(with).unwrap();
    let net = e2.network.expect("network present");
    assert_eq!(net.mode, McpNetMode::Restricted);
    assert_eq!(net.allow_hosts, vec!["example.com", "*.api.example.com"]);

    // Round-trip keeps None out of the serialized form.
    let out = serde_yaml_ng::to_string(&e).unwrap();
    assert!(!out.contains("network"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-common mcp_entry_network`
Expected: FAIL — `McpServerNetwork` / `network` field do not exist (compile error).

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpNetMode {
    /// Inherit the agent-level outbound policy (today's behavior). No proxy.
    #[default]
    Inherit,
    /// Allow only `allow_hosts`, routed through the runtime egress proxy.
    Restricted,
    /// No outbound for this server at all.
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerNetwork {
    #[serde(default)]
    pub mode: McpNetMode,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}
```

Add to `McpServerEntry` (after `timeout_secs`):

```rust
    /// Per-server outbound egress override. `None` = inherit the agent policy
    /// (default; unchanged behavior). See docs/superpowers/plans/2026-06-26-…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<McpServerNetwork>,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-common mcp_entry_network`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs
git commit -m "feat(agent): per-MCP-server network egress schema (McpServerNetwork)"
```

---

### Task 2: Host-allowlist matcher (reuse)

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/reqwest_guard.rs` (add a thin `host_allowed` over the existing `host_matches_pattern`).
- Test: inline in the same file.

**Interfaces:**
- Consumes: `host_matches_pattern(host: &str, pattern: &str) -> bool` (already `pub`, line 13).
- Produces: `pub fn host_allowed(host: &str, allow: &[String]) -> bool` — empty allowlist ⇒ deny-all (fail-closed).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn host_allowed_is_fail_closed_and_pattern_aware() {
    let allow = vec!["example.com".to_string(), "*.api.example.com".to_string()];
    assert!(host_allowed("example.com", &allow));
    assert!(host_allowed("v1.api.example.com", &allow));
    assert!(!host_allowed("evil.com", &allow));
    assert!(!host_allowed("example.com", &[]), "empty allowlist denies");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime host_allowed_is_fail_closed`
Expected: FAIL — `host_allowed` not defined.

- [ ] **Step 3: Implement**

```rust
/// True if `host` matches any allowlist pattern. An empty allowlist denies all
/// (fail-closed). Reuses the same matcher the agent's reqwest guard uses.
pub fn host_allowed(host: &str, allow: &[String]) -> bool {
    allow.iter().any(|p| host_matches_pattern(host, p))
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime host_allowed_is_fail_closed`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/reqwest_guard.rs
git commit -m "feat(sandbox): host_allowed allowlist matcher (fail-closed)"
```

---

### Task 3: Loopback egress proxy (CONNECT, token→allowlist, audit)

**Files:**
- Create: `mur-agent-runtime/src/sandbox/egress_proxy.rs`
- Modify: `mur-agent-runtime/src/sandbox/mod.rs` (add `pub mod egress_proxy;`)
- Test: inline `#[cfg(test)]` in `egress_proxy.rs` (uses a loopback echo upstream).

**Interfaces:**
- Consumes: `host_allowed` (Task 2).
- Produces:
  - `pub struct EgressProxyHandle { pub addr: std::net::SocketAddr, /* + token registry */ }`
  - `impl EgressProxyHandle { pub fn register(&self, allow_hosts: Vec<String>) -> String /* token */ }`
  - `pub async fn start_egress_proxy() -> std::io::Result<EgressProxyHandle>` — binds `127.0.0.1:0`, spawns the accept loop, returns the handle (with the chosen ephemeral port).

**Design:** one shared proxy. Each policied child is given `HTTP_PROXY=http://<token>:x@127.0.0.1:<port>`; the proxy reads `Proxy-Authorization: Basic base64(<token>:x)` on the `CONNECT host:port` request, looks the token up to get that server's allowlist, and tunnels only if `host_allowed`. Unknown/missing token ⇒ 403. Every decision is logged (`tracing`) for audit.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use base64::Engine;

    // A trivial upstream that accepts a connection and echoes one line.
    async fn echo_upstream() -> std::net::SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = l.accept().await {
                let mut b = [0u8; 16];
                let n = s.read(&mut b).await.unwrap_or(0);
                let _ = s.write_all(&b[..n]).await;
            }
        });
        addr
    }

    async fn connect_via(proxy: std::net::SocketAddr, token: &str, target: &str) -> String {
        let mut s = TcpStream::connect(proxy).await.unwrap();
        let cred = base64::engine::general_purpose::STANDARD.encode(format!("{token}:x"));
        let req = format!(
            "CONNECT {target} HTTP/1.1\r\nProxy-Authorization: Basic {cred}\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    #[tokio::test]
    async fn allowed_host_tunnels_denied_host_403() {
        let up = echo_upstream().await;
        let proxy = start_egress_proxy().await.unwrap();
        // Allowlist only the upstream's loopback host.
        let token = proxy.register(vec!["127.0.0.1".to_string()]);

        let ok = connect_via(proxy.addr, &token, &up.to_string()).await;
        assert!(ok.starts_with("HTTP/1.1 200"), "allowed CONNECT establishes: {ok}");

        // A token whose allowlist does not include the target → 403.
        let token2 = proxy.register(vec!["example.com".to_string()]);
        let denied = connect_via(proxy.addr, &token2, &up.to_string()).await;
        assert!(denied.starts_with("HTTP/1.1 403"), "denied CONNECT is 403: {denied}");

        // Unknown token → 403.
        let bad = connect_via(proxy.addr, "not-a-real-token", &up.to_string()).await;
        assert!(bad.starts_with("HTTP/1.1 403"), "unknown token is 403: {bad}");
    }
}
```

> Note: this introduces `base64` (already in the dependency tree via reqwest/others — confirm with `cargo tree -p mur-agent-runtime -i base64`; if absent, encode the `token:x` cred by hand in the test to avoid a new dep).

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime egress_proxy::tests::allowed_host_tunnels`
Expected: FAIL — `start_egress_proxy` / `EgressProxyHandle` do not exist.

- [ ] **Step 3: Implement the proxy**

```rust
//! Loopback egress proxy for per-MCP-server host allowlisting. ADVISORY
//! enforcement: a cooperating child honors HTTP_PROXY and is constrained to its
//! allowlist; a child that ignores HTTP_PROXY can still reach the network
//! directly (the OS sandbox here filters by port, not host). Airtight
//! containment is future work (Linux netns + macOS pre-fork launcher).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::reqwest_guard::host_allowed;

type Registry = Arc<Mutex<HashMap<String, Vec<String>>>>;

#[derive(Clone)]
pub struct EgressProxyHandle {
    pub addr: SocketAddr,
    registry: Registry,
}

impl EgressProxyHandle {
    /// Register a per-server allowlist; returns the bearer token to embed in the
    /// child's HTTP_PROXY credentials.
    pub fn register(&self, allow_hosts: Vec<String>) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.registry.lock().unwrap().insert(token.clone(), allow_hosts);
        token
    }
}

pub async fn start_egress_proxy() -> std::io::Result<EgressProxyHandle> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
    let reg = registry.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { continue };
            let reg = reg.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_conn(sock, reg).await {
                    tracing::debug!("egress proxy conn ended: {e}");
                }
            });
        }
    });
    Ok(EgressProxyHandle { addr, registry })
}

async fn handle_conn(mut client: TcpStream, registry: Registry) -> std::io::Result<()> {
    // Read the request head (CONNECT line + headers, up to the blank line).
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
        if client.read(&mut byte).await? == 0 { return Ok(()); }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    // Only CONNECT (https) is supported in this MVP; plain http is Task 3b.
    let Some(target) = request_line.strip_prefix("CONNECT ").and_then(|r| r.split(' ').next())
    else {
        client.write_all(b"HTTP/1.1 501 Not Implemented\r\n\r\n").await?;
        return Ok(());
    };
    let token = lines
        .find_map(|l| l.strip_prefix("Proxy-Authorization: Basic "))
        .and_then(decode_basic_user); // returns the "token" part of token:x

    let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(target);
    let allowed = token
        .as_deref()
        .and_then(|t| registry.lock().unwrap().get(t).cloned())
        .map(|allow| host_allowed(host, &allow))
        .unwrap_or(false);

    if !allowed {
        tracing::info!(host, "egress proxy DENY");
        client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
        return Ok(());
    }
    tracing::info!(host, "egress proxy ALLOW");
    let mut upstream = TcpStream::connect(target).await?;
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// Decode `Basic base64(user:pass)` and return `user` (our token). The password
/// half is a throwaway `x`.
fn decode_basic_user(b64: &str) -> Option<String> {
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    let s = String::from_utf8(raw).ok()?;
    Some(s.split_once(':').map(|(u, _)| u).unwrap_or(&s).to_string())
}
```

Add to `mur-agent-runtime/src/sandbox/mod.rs`:

```rust
pub mod egress_proxy;
```

- [ ] **Step 4: Run to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime egress_proxy::tests::allowed_host_tunnels`
Expected: PASS (allowed → 200, denied → 403, unknown token → 403).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/egress_proxy.rs mur-agent-runtime/src/sandbox/mod.rs
git commit -m "feat(sandbox): loopback CONNECT egress proxy with per-token host allowlist"
```

---

### Task 4: Inject the proxy into policied MCP child spawns

**Files:**
- Modify: `mur-agent-runtime/src/protocol/mcp_client.rs:50-60` (`McpClient::spawn`) to set env when the entry has a `Restricted` policy.
- Modify: `mur-agent-runtime/src/mcp/pool.rs` (`McpPool` carries an `Option<EgressProxyHandle>`; passes it to `spawn`).
- Modify: the `McpPool` construction site (per Explore: pool built in the supervisor) to start the proxy lazily iff any agent server has a `Restricted` policy.
- Test: inline test on the pure env-building helper (below) — the spawn itself is process glue.

**Interfaces:**
- Consumes: `McpServerEntry.network` (Task 1), `EgressProxyHandle` (Task 3).
- Produces: `pub fn proxy_env_for(entry: &McpServerEntry, proxy: Option<&EgressProxyHandle>) -> Vec<(String, String)>` in `mcp_client.rs` — the env pairs to set on the child (empty unless `mode == Restricted`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn proxy_env_only_for_restricted_servers() {
    use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};
    let base = McpServerEntry { name: "x".into(), command: "npx".into(), ..Default::default() };

    // No policy → no env (byte-for-byte today).
    assert!(proxy_env_for(&base, None).is_empty());

    // Restricted but no proxy handle → still empty (defensive).
    let mut restricted = base.clone();
    restricted.network = Some(McpServerNetwork { mode: McpNetMode::Restricted, allow_hosts: vec!["example.com".into()] });
    assert!(proxy_env_for(&restricted, None).is_empty());

    // Restricted + proxy → HTTP_PROXY/HTTPS_PROXY/NO_PROXY set, loopback in NO_PROXY.
    let handle = test_handle("127.0.0.1:9".parse().unwrap());
    let env = proxy_env_for(&restricted, Some(&handle));
    let map: std::collections::HashMap<_, _> = env.into_iter().collect();
    assert!(map.get("HTTP_PROXY").unwrap().contains("@127.0.0.1:9"));
    assert!(map.get("HTTPS_PROXY").unwrap().contains("@127.0.0.1:9"));
    assert!(map.get("NO_PROXY").unwrap().contains("127.0.0.1"));
}
```

(`test_handle` builds an `EgressProxyHandle` with an empty registry at a given addr — add a `#[cfg(test)] pub fn for_test(addr) -> EgressProxyHandle` to Task 3's type.)

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime proxy_env_only_for_restricted`
Expected: FAIL — `proxy_env_for` not defined.

- [ ] **Step 3: Implement**

```rust
/// Env vars to scope a policied MCP child to its allowlist via the egress proxy.
/// Empty (no change vs today) unless the server is `Restricted` AND a proxy is
/// available. NEVER set these on the runtime's own environment — only on the
/// child Command (Global Constraint: env isolation).
pub fn proxy_env_for(
    entry: &McpServerEntry,
    proxy: Option<&crate::sandbox::egress_proxy::EgressProxyHandle>,
) -> Vec<(String, String)> {
    let (Some(net), Some(proxy)) = (entry.network.as_ref(), proxy) else { return vec![] };
    if net.mode != mur_common::agent::McpNetMode::Restricted {
        return vec![];
    }
    let token = proxy.register(net.allow_hosts.clone());
    let url = format!("http://{token}:x@{}", proxy.addr);
    vec![
        ("HTTP_PROXY".into(), url.clone()),
        ("HTTPS_PROXY".into(), url.clone()),
        ("http_proxy".into(), url.clone()),
        ("https_proxy".into(), url),
        ("NO_PROXY".into(), "127.0.0.1,localhost,::1".into()),
        ("no_proxy".into(), "127.0.0.1,localhost,::1".into()),
    ]
}
```

Then in `McpClient::spawn` (mcp_client.rs:50-57), after building `std_cmd`:

```rust
    for (k, v) in proxy_env_for(entry, proxy) {
        std_cmd.env(k, v);
    }
```

Thread `proxy: Option<&EgressProxyHandle>` through `McpClient::spawn` and `McpPool::client`; the pool stores `Option<EgressProxyHandle>` set at construction. At pool construction, start the proxy iff `entries.iter().any(|e| matches!(e.network.as_ref().map(|n| n.mode), Some(McpNetMode::Restricted)))`.

- [ ] **Step 4: Run to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime proxy_env_only_for_restricted`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/protocol/mcp_client.rs mur-agent-runtime/src/mcp/pool.rs mur-agent-runtime/src/supervisor.rs
git commit -m "feat(mcp): route Restricted MCP servers through the egress proxy (per-child env only)"
```

---

### Task 5: Isolate the agent LLM clients from ambient proxy (the cc-proxy guarantee)

**Files:**
- Modify: `mur-agent-runtime/src/llm/anthropic.rs:73`, `mur-agent-runtime/src/llm/openai.rs:34`, `mur-agent-runtime/src/llm/ollama.rs:20` (add `.no_proxy()` to each `reqwest::Client::builder()`).
- Test: `mur-agent-runtime/src/llm/anthropic.rs` inline.

**Interfaces:** none new — behavior guarantee only.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn anthropic_client_ignores_ambient_http_proxy() {
    // Even with HTTP_PROXY set in the environment (as it is for any policied
    // MCP child, and as cc-proxy debugging might set globally), the agent's
    // Anthropic client must NOT route through it — it talks to base_url only.
    // SAFETY: single-threaded test; restore after.
    unsafe { std::env::set_var("HTTP_PROXY", "http://127.0.0.1:1/"); }
    let client = AnthropicClient::new(/* minimal ctor args / base_url */);
    // The builder used `.no_proxy()`, so reqwest records no proxy.
    assert!(client.has_no_proxy(), "LLM client must be built with .no_proxy()");
    unsafe { std::env::remove_var("HTTP_PROXY"); }
}
```

> `has_no_proxy()` is a tiny test-only accessor reflecting that the client was constructed via the `.no_proxy()` path. If exposing reqwest internals is awkward, instead assert at the construction helper: extract `fn llm_client_builder() -> reqwest::ClientBuilder` that all three call, and unit-test that the helper sets no_proxy by checking it builds without reading env (document the guarantee with a comment + the shared helper as the single enforcement point).

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime anthropic_client_ignores_ambient_http_proxy`
Expected: FAIL — builder does not set `.no_proxy()` (no accessor / guarantee).

- [ ] **Step 3: Implement**

Add `.no_proxy()` to each builder, e.g. anthropic.rs:

```rust
let http = reqwest::Client::builder()
    .no_proxy() // isolation: never inherit ambient HTTP_PROXY (cc-proxy + per-server proxy live elsewhere)
    // …existing .dns_resolver(HostGuard…)/.timeout()/etc unchanged…
    .build()?;
```

Repeat for `openai.rs:34` and `ollama.rs:20`. (Prefer extracting a shared `llm_client_builder()` if the three builders are otherwise identical — DRY — and add `.no_proxy()` once there.)

- [ ] **Step 4: Run to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime anthropic_client_ignores_ambient_http_proxy`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/llm/anthropic.rs mur-agent-runtime/src/llm/openai.rs mur-agent-runtime/src/llm/ollama.rs
git commit -m "fix(llm): build agent LLM clients with .no_proxy() (isolate from egress/cc-proxy)"
```

---

### Task 6: Surface — set a per-server policy from the CLI

**Files:**
- Modify: `mur-core/src/cmd/agent/mcp.rs` (add `cmd_mcp_set_network`), `mur-core/src/cli/agent.rs` (`AgentMcpAction::SetNetwork`), `mur-core/src/dispatch.rs` (dispatch arm).
- Test: `mur-core` inline test on the profile mutation.

**Interfaces:**
- Consumes: `McpServerNetwork`, `McpNetMode` (Task 1).
- Produces: `pub fn cmd_mcp_set_network(agent: &str, server_id: &str, allow_hosts: Vec<String>, off: bool) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn set_network_writes_restricted_allowlist() {
    let _g = TEST_HOME_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("MUR_HOME", tmp.path()); }
    write_agent_with_mcp(tmp.path(), "rustsmith", "browser", "npx");

    cmd_mcp_set_network("rustsmith", "browser", vec!["example.com".into()], false).unwrap();

    let (_p, profile) = load_profile_for_edit("rustsmith").unwrap();
    let srv = profile.mcp_servers.iter().find(|s| s.name == "browser").unwrap();
    let net = srv.network.as_ref().unwrap();
    assert_eq!(net.mode, McpNetMode::Restricted);
    assert_eq!(net.allow_hosts, vec!["example.com"]);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core set_network_writes_restricted`
Expected: FAIL — `cmd_mcp_set_network` not defined.

- [ ] **Step 3: Implement**

```rust
/// Set (or clear) a per-server egress policy. `off=true` ⇒ McpNetMode::Off;
/// a non-empty allowlist ⇒ Restricted; empty allowlist + !off ⇒ clear to None
/// (inherit the agent policy).
pub fn cmd_mcp_set_network(
    agent: &str,
    server_id: &str,
    allow_hosts: Vec<String>,
    off: bool,
) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    let srv = profile
        .mcp_servers
        .iter_mut()
        .find(|s| s.name == server_id)
        .ok_or_else(|| anyhow::anyhow!("MCP server '{server_id}' not found on '{agent}'"))?;
    srv.network = if off {
        Some(McpServerNetwork { mode: McpNetMode::Off, allow_hosts: vec![] })
    } else if allow_hosts.is_empty() {
        None
    } else {
        Some(McpServerNetwork { mode: McpNetMode::Restricted, allow_hosts })
    };
    save_profile(&path, &profile)?;
    println!("Updated egress policy for '{server_id}'. Restart the agent to apply.");
    Ok(())
}
```

Add `AgentMcpAction::SetNetwork { name: String, server_id: String, #[arg(long="allow-host")] allow_hosts: Vec<String>, #[arg(long)] off: bool }` and a dispatch arm calling `cmd_mcp_set_network`.

- [ ] **Step 4: Run to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core set_network_writes_restricted`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/mcp.rs mur-core/src/cli/agent.rs mur-core/src/dispatch.rs
git commit -m "feat(cli): mur agent mcp set-network --allow-host (per-server egress)"
```

---

### Task 7: Threat-model + operator docs

**Files:**
- Modify: `docs/architecture/runtime-overview.md` (new "Per-server MCP egress" subsection).
- Test: `cargo run -- verify --file docs/architecture/runtime-overview.md` (stale-claim scan) passes.

**Interfaces:** none.

- [ ] **Step 1: Write the docs**

Add a subsection stating, verbatim:
- What it does: opt-in per-server host allowlist via a loopback CONNECT proxy; `mur agent mcp set-network --allow-host`.
- Isolation guarantee: child-scoped `HTTP_PROXY` only; agent LLM clients use `.no_proxy()`; cc-proxy/base_url path untouched; loopback in `NO_PROXY`.
- **Threat model (advisory):** enforces against cooperating tools; a tool that ignores `HTTP_PROXY` can still reach the network directly because the OS sandbox filters by port, not host. Airtight containment requires Linux network-namespace isolation + a macOS pre-fork launcher (the `sandbox/child.rs:13-25` limitation) — explicitly out of scope here.
- Audit: every allow/deny is logged via `tracing` at the proxy.

- [ ] **Step 2: Verify docs are consistent**

Run: `cargo run -- verify --file docs/architecture/runtime-overview.md`
Expected: no stale-claim errors for the new section.

- [ ] **Step 3: Commit**

```bash
git add docs/architecture/runtime-overview.md
git commit -m "docs(runtime): per-server MCP egress — usage + advisory threat model"
```

---

## Self-Review

**Spec coverage (Plan B — proxy, advisory):**
- Per-server schema → Task 1. ✓
- Host allowlist enforcement → Task 2 (matcher) + Task 3 (proxy). ✓
- Children routed through proxy, opt-in, per-child env only → Task 4. ✓
- cc-proxy / LLM isolation guarantee (the user's explicit concern) → Task 5 (regression test) + Global Constraints. ✓
- Operator surface to set policy → Task 6. ✓
- Honest advisory threat model + airtight deferral (netns / macOS launcher) → Task 7 + Architecture sentence. ✓
- Opt-in zero-change default → enforced in Task 1 (None) + Task 4 (empty env) + tested. ✓

**Deferred (intentional, documented):** plain-HTTP forwarding in the proxy (MVP is CONNECT/https only — Task 3b follow-up); airtight OS containment (Linux netns + macOS pre-fork launcher); Hub UI field for the allowlist (CLI-first; Hub field is a fast-follow mirroring Task 6 like the discover/import work). Privileged broker for non-network system tools (ssh/desktop-commander) is a **sibling** effort, not this plan.

**Placeholder scan:** every code step shows real code; the only soft spot is Task 5's `has_no_proxy()` test accessor — the step gives a concrete fallback (shared `llm_client_builder()` helper as the single enforcement point) if reqwest internals can't be asserted directly.

**Type consistency:** `McpServerNetwork`/`McpNetMode`/`McpServerEntry.network` (Task 1) are used identically in Tasks 4 and 6; `EgressProxyHandle`/`register`/`addr` (Task 3) match their uses in Task 4; `host_allowed` (Task 2) matches its use in Task 3.
