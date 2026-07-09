# MUR-Native Deep Research Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give MUR a native deep-research capability (decompose → parallel fan-out → adversarial verify → cited synthesis) that runs on MUR's own fleet + agents + dynamic router loop, with all web egress funneled through one deterministic, audited, SSRF-guarded MCP gateway.

**Architecture:** A new `mur-research-gateway` MCP server (Rust, MUR-shipped, no LLM) is the single trust boundary that holds `broad-audited` egress; it exposes read-only `search`/`fetch` verbs with a 3-tier escalation ladder (reqwest → agent-browser/lightpanda → chrome). A `deep-research` fleet of restricted worker agents mounts only that gateway; the fleet router drives a dynamic per-iteration DAG (decompose/research/verify/synthesize) via `mur fleet run --loop`, converging on a `done_when` marker.

**Tech Stack:** Rust (workspace crate), stdio JSON-RPC MCP, `reqwest`, `agent-browser` (npm, Apache-2.0) + Lightpanda (AGPL-3.0, subprocess-only), MUR fleet machinery, per-server egress governance (#661).

## Global Constraints

- **No hardcoded values.** Lightpanda executable path, engine defaults, worker count, per-request timeout, `deny_hosts` overlay, search result limits — all from `~/.mur/config.yaml` / fleet config / env, never literals. — CLAUDE.md rule 1.
- **Lightpanda (AGPL-3.0) is subprocess-only** — invoked via `agent-browser --engine lightpanda`, never linked in-process, never forked/modified; ship the unmodified upstream binary + AGPL attribution. — spec §5, `gotcha_agent_browser_lightpanda_engine_dead`.
- **`agent-browser --engine lightpanda` requires `--args ""`** — Chrome stealth args must never reach Lightpanda (it errors). Chrome tier keeps stealth args. — verified 0.31.1, 2026-07-08.
- **`agent-browser mcp` / driver requires >= 0.28.0.** Preflight must check and degrade explicitly if lower/missing. — verified 2026-07-08.
- **Workers hold NO egress.** Only the gateway subprocess reaches the web. Worker agent entitlements stay `restricted`. — spec §3.
- **Egress grant only via the shipped consent path** (`mur agent mcp set-network <agent> research-gateway --broad-audited`). Fleet creation never opens egress implicitly. — spec §7.1.
- **SSRF guard is non-configurable and stricter than the runtime's local-first guard** — the gateway refuses loopback + RFC1918/ULA private + link-local/metadata + unspecified (the runtime's `is_link_local_or_unspecified` deliberately allows loopback/private for local LLMs; the gateway must NOT). The two IP predicates stay separate (different policies); the host-pattern **matcher** is single-sourced in `mur-common::net` (shared by the egress proxy and the gateway — a security boundary must not have two copies that can drift). — spec §5, §7.3.
- **Brand:** user-facing label is uppercase where surfaced; internal names lowercase. — CLAUDE.md rule 7.
- **Build env:** `mur-core`/runtime need `ORT_STRATEGY=download` + `MUR_WEB_DIST=$HOME/Projects/mur-web/dist`; rustup toolchain on PATH. The gateway crate itself is light (no ORT). — `mem:env_ort_strategy_download`.

---

### Task 1: Scaffold `mur-research-gateway` crate (stdio MCP, tool declarations only)

Stand up the binary and JSON-RPC loop with `search`/`fetch` declared but returning "not implemented", so the MCP handshake is testable before any logic exists. Mirrors `mur-mcp-server` structure exactly.

**Files:**
- Create: `mur-research-gateway/Cargo.toml`
- Create: `mur-research-gateway/src/main.rs`
- Create: `mur-research-gateway/src/jsonrpc.rs`
- Create: `mur-research-gateway/src/server.rs`
- Create: `mur-research-gateway/src/tools.rs`
- Modify: `Cargo.toml` (workspace `members` — add `"mur-research-gateway"` after `"mur-mcp-server"`)

**Interfaces:**
- Produces: a binary `mur-research-gateway` speaking stdio JSON-RPC MCP, advertising two tools: `search {query: string, limit?: number}` and `fetch {url: string, render?: boolean}`. Tool dispatch entry point `server::McpServer::handle(Request) -> Response`.

- [ ] **Step 1: Copy the JSON-RPC framing verbatim from mur-mcp-server**

Run: `cp mur-mcp-server/src/jsonrpc.rs mur-research-gateway/src/jsonrpc.rs` (create the dir first with the Cargo.toml step). This module (`read_request`/`write_response`, `Request`/`Response` types) is transport-only and reused unchanged.

- [ ] **Step 2: Write `Cargo.toml`**

```toml
[package]
name = "mur-research-gateway"
version.workspace = true
edition.workspace = true

[[bin]]
name = "mur-research-gateway"
path = "src/main.rs"

[dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "io-std", "process"] }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
reqwest = { workspace = true }
mur-common = { path = "../mur-common" }
```

(If a listed dependency is not yet in `[workspace.dependencies]`, mirror the version used by `mur-mcp-server`/`mur-agent-runtime` Cargo.toml.)

- [ ] **Step 3: Add the crate to the workspace members**

Modify root `Cargo.toml` `members` list, adding `"mur-research-gateway",` after `"mur-mcp-server",`.

- [ ] **Step 4: Write `tools.rs` — tool declarations**

```rust
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")] // MCP wire protocol is camelCase
    pub input_schema: serde_json::Value,
}

/// The two read-only verbs workers may call. No navigation, no POST.
pub fn all_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "search".into(),
            description: "Web search. Returns [{title,url,snippet}]. Read-only.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "number"}
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "fetch".into(),
            description: "Fetch one URL's readable text. Read-only GET. SSRF-guarded.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "render": {"type": "boolean"}
                },
                "required": ["url"]
            }),
        },
    ]
}
```

- [ ] **Step 5: Write `server.rs` — handshake + dispatch stubs**

```rust
use crate::jsonrpc::{Request, Response};
use crate::tools;

pub struct McpServer;

impl McpServer {
    pub fn new() -> Self { McpServer }

    pub async fn handle(&mut self, req: Request) -> Response {
        match req.method.as_str() {
            "initialize" => Response::result(req.id, serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mur-research-gateway", "version": env!("CARGO_PKG_VERSION")}
            })),
            "tools/list" => Response::result(req.id, serde_json::json!({
                "tools": tools::all_tools()
            })),
            "tools/call" => Response::error(req.id, -32601, "not implemented yet"),
            _ => Response::error(req.id, -32601, "method not found"),
        }
    }
}
```

(Match `Response::result` / `Response::error` to the actual constructors in the copied `jsonrpc.rs`; adjust if their signatures differ.)

- [ ] **Step 6: Write `main.rs` — the stdio loop (mirror mur-mcp-server)**

```rust
use tracing_subscriber::EnvFilter;
mod jsonrpc;
mod server;
mod tools;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with_writer(std::io::stderr)
        .init();
    let mut server = server::McpServer::new();
    while let Some(request) = jsonrpc::read_request() {
        let is_notification = request.id.is_none() && request.method.starts_with("notifications/");
        let response = server.handle(request).await;
        if !is_notification { jsonrpc::write_response(&response); }
    }
}
```

- [ ] **Step 7: Build and smoke-test the handshake**

Run:
```bash
cargo build -p mur-research-gateway
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' | ./target/debug/mur-research-gateway
```
Expected: two JSON-RPC responses on stdout; the second lists `search` and `fetch`.

- [ ] **Step 8: Commit**

```bash
git add mur-research-gateway Cargo.toml
git commit -m "feat(research-gateway): scaffold stdio MCP with search/fetch declarations"
```

---

### Task 2: shared host-matching (mur-common) + strict SSRF guard

The security core. Two phases: (A) hoist the host-pattern matcher to `mur-common`
so the egress proxy and this gateway share ONE matcher (a security boundary must
not have two copies that can drift); (B) build the gateway's guard on top. The
SSRF **IP** predicate is deliberately NOT shared — the runtime's
`is_link_local_or_unspecified` allows loopback/RFC1918 (local LLMs) while the
gateway forbids them; these are two different policies, not one duplicated block.

**Files:**
- Create: `mur-common/src/net.rs` (pure host-pattern matcher, shared)
- Modify: `mur-common/src/lib.rs` (`pub mod net;`)
- Modify: `mur-agent-runtime/src/sandbox/reqwest_guard.rs` (delete local `host_matches_pattern`/`host_allowed`; `pub use mur_common::net::{host_matches_pattern, host_allowed};` so existing callers are unchanged)
- Create: `mur-research-gateway/src/net_guard.rs`
- Modify: `mur-research-gateway/src/main.rs` (add `mod net_guard;`)
- Modify: `mur-research-gateway/Cargo.toml` (add `url = { workspace = true }`)

**Interfaces:**
- Produces (mur-common):
  - `pub fn host_matches_pattern(host: &str, pattern: &str) -> bool` — `*.x.com` / legacy `.x.com` match subdomains + apex; else exact. Byte-identical to the current reqwest_guard version.
  - `pub fn host_allowed(host: &str, allow: &[String]) -> bool` — true if any pattern matches (empty = false).
- Produces (gateway `net_guard`):
  - `fn is_forbidden_target(ip: std::net::IpAddr) -> bool` — true for loopback, RFC1918/ULA private, link-local/metadata, unspecified (normalizing IPv4-in-IPv6 first).
  - `fn host_denied(host: &str, deny: &[String]) -> bool` — thin wrapper: `mur_common::net::host_allowed(host, deny)` (same matcher; the deny LIST gives it deny semantics).
  - `fn screen_url(url: &str, deny: &[String]) -> Result<url::Url, GuardReject>` — parse, reject non-http(s), reject denied host, resolve host and reject if ANY resolved IP is a forbidden target. Returns the parsed URL on pass.
  - `enum GuardReject { BadScheme, DeniedHost, PrivateAddress, Unresolvable }`

- [ ] **Step 1: Move the matcher to mur-common**

Create `mur-common/src/net.rs` with `host_matches_pattern` + `host_allowed` copied **verbatim** from `mur-agent-runtime/src/sandbox/reqwest_guard.rs` (lines ~12-33), plus a unit test for each. Add `pub mod net;` to `mur-common/src/lib.rs`.

- [ ] **Step 2: Point reqwest_guard at the shared matcher**

In `reqwest_guard.rs`, delete the two local fns and add `pub use mur_common::net::{host_matches_pattern, host_allowed};` at the top (callers in `egress_proxy.rs` import them from here — unchanged). This is behavior-preserving.

- [ ] **Step 3: Verify no egress regression, commit the extraction**

Run: `ORT_STRATEGY=download cargo test -p mur-common net:: && ORT_STRATEGY=download cargo test -p mur-agent-runtime egress_proxy`
Expected: PASS (matcher moved, egress-proxy allow/deny behavior identical).
```bash
git add mur-common/src/net.rs mur-common/src/lib.rs mur-agent-runtime/src/sandbox/reqwest_guard.rs
git commit -m "refactor(net): hoist host-pattern matcher to mur-common (single source for egress + gateway)"
```

- [ ] **Step 4: Write the failing gateway-guard tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn blocks_cloud_metadata_and_private_and_loopback() {
        for ip in ["169.254.169.254", "127.0.0.1", "10.0.0.5", "192.168.1.1", "172.16.0.1", "::1", "fe80::1", "::ffff:169.254.169.254"] {
            assert!(is_forbidden_target(ip.parse::<IpAddr>().unwrap()), "{ip} must be forbidden");
        }
    }
    #[test]
    fn allows_public() {
        for ip in ["8.8.8.8", "203.0.113.7", "2606:4700:4700::1111"] {
            assert!(!is_forbidden_target(ip.parse::<IpAddr>().unwrap()), "{ip} must be allowed");
        }
    }
    #[test]
    fn deny_host_patterns() {
        let deny = vec!["*.internal.corp".to_string(), "blocked.example".to_string()];
        assert!(host_denied("api.internal.corp", &deny));
        assert!(host_denied("internal.corp", &deny));
        assert!(host_denied("blocked.example", &deny));
        assert!(!host_denied("good.example", &deny));
    }
    #[test]
    fn screen_rejects_bad_scheme_and_denied() {
        assert!(matches!(screen_url("file:///etc/passwd", &[]), Err(GuardReject::BadScheme)));
        assert!(matches!(screen_url("http://blocked.example/", &["blocked.example".into()]), Err(GuardReject::DeniedHost)));
    }
}
```

- [ ] **Step 5: Run to verify they fail**

Run: `cargo test -p mur-research-gateway net_guard`
Expected: FAIL — `net_guard` unresolved.

- [ ] **Step 6: Implement `net_guard.rs`**

```rust
use std::net::{IpAddr, ToSocketAddrs};

#[derive(Debug, PartialEq)]
pub enum GuardReject { BadScheme, DeniedHost, PrivateAddress, Unresolvable }

/// Deny semantics via the shared matcher (single source of truth with the
/// egress proxy — `mur_common::net`). Same matcher; the deny LIST is what
/// makes a match mean "blocked".
pub fn host_denied(host: &str, deny: &[String]) -> bool {
    mur_common::net::host_allowed(host, deny)
}

/// STRICTER than the runtime's local-first guard: a web researcher has no
/// legitimate reason to reach loopback/private/link-local, so all are forbidden.
pub fn is_forbidden_target(ip: IpAddr) -> bool {
    // Normalize IPv4-in-IPv6 (mapped ::ffff:a.b.c.d and compatible ::a.b.c.d).
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local()
                || v4.is_unspecified() || v4.is_broadcast()
                || v4.octets()[0] == 0 // 0.0.0.0/8
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified()
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
        }
    }
}

/// Parse + screen a URL. On pass returns the parsed URL; the caller MUST pin
/// its connection to an already-screened address (see Task 3) to close the
/// resolve→connect TOCTOU (DNS-rebinding) window.
pub fn screen_url(raw: &str, deny: &[String]) -> Result<url::Url, GuardReject> {
    let u = url::Url::parse(raw).map_err(|_| GuardReject::BadScheme)?;
    if !matches!(u.scheme(), "http" | "https") { return Err(GuardReject::BadScheme); }
    let host = u.host_str().ok_or(GuardReject::BadScheme)?;
    if host_denied(host, deny) { return Err(GuardReject::DeniedHost); }
    let port = u.port_or_known_default().unwrap_or(80);
    let mut any = false;
    for sa in (host, port).to_socket_addrs().map_err(|_| GuardReject::Unresolvable)? {
        any = true;
        if is_forbidden_target(sa.ip()) { return Err(GuardReject::PrivateAddress); }
    }
    if !any { return Err(GuardReject::Unresolvable); }
    Ok(u)
}
```

- [ ] **Step 7: Run to verify they pass**

Run: `cargo test -p mur-research-gateway net_guard`
Expected: PASS (all four tests).

- [ ] **Step 8: Commit**

```bash
git add mur-research-gateway/src/net_guard.rs mur-research-gateway/src/main.rs mur-research-gateway/Cargo.toml
git commit -m "feat(research-gateway): strict SSRF guard reusing mur-common host matcher"
```

---

### Task 3: tier-1 `fetch(url)` — SSRF-pinned reqwest GET

Implement the cheapest tier: a plain GET that reuses `screen_url` and pins the connection to the screened IP (closing the resolve→connect TOCTOU).

**Files:**
- Create: `mur-research-gateway/src/fetcher.rs`
- Modify: `mur-research-gateway/src/main.rs` (`mod fetcher;`)
- Modify: `mur-research-gateway/src/server.rs` (wire `fetch` in `tools/call`)

**Interfaces:**
- Consumes: `net_guard::screen_url`.
- Produces:
  - `struct FetchResult { url: String, status: u16, title: Option<String>, text: String, tier: u8 }`
  - `async fn fetch_tier1(url: &str, deny: &[String], timeout: std::time::Duration) -> Result<FetchResult, FetchError>`
  - `enum FetchError { Guard(net_guard::GuardReject), Http(String), TooLarge }`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn refuses_private_target() {
        let e = fetch_tier1("http://127.0.0.1:1/", &[], Duration::from_secs(2)).await.unwrap_err();
        assert!(matches!(e, FetchError::Guard(_)));
    }
    #[tokio::test]
    async fn refuses_denied_host() {
        let e = fetch_tier1("http://blocked.example/", &["blocked.example".into()], Duration::from_secs(2)).await.unwrap_err();
        assert!(matches!(e, FetchError::Guard(net_guard::GuardReject::DeniedHost)));
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p mur-research-gateway fetcher`
Expected: FAIL — `fetcher` unresolved.

- [ ] **Step 3: Implement `fetcher.rs`**

```rust
use crate::net_guard::{self, GuardReject};
use std::time::Duration;

const MAX_BODY_BYTES: usize = 5 * 1024 * 1024; // ponytail: 5MB cap; config if a real doc exceeds it

#[derive(Debug)]
pub struct FetchResult { pub url: String, pub status: u16, pub title: Option<String>, pub text: String, pub tier: u8 }

#[derive(Debug)]
pub enum FetchError { Guard(GuardReject), Http(String), TooLarge }

pub async fn fetch_tier1(url: &str, deny: &[String], timeout: Duration) -> Result<FetchResult, FetchError> {
    let screened = net_guard::screen_url(url, deny).map_err(FetchError::Guard)?;
    // ponytail: reqwest re-resolves internally; screen_url already rejected
    // private targets. A pinned-IP resolver closes the rebinding window fully —
    // upgrade to reqwest::dns::Resolve if the advisory window matters.
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none()) // no auto-redirect: each hop must be re-screened by the worker
        .build().map_err(|e| FetchError::Http(e.to_string()))?;
    let resp = client.get(screened.clone()).send().await.map_err(|e| FetchError::Http(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp.text().await.map_err(|e| FetchError::Http(e.to_string()))?;
    if body.len() > MAX_BODY_BYTES { return Err(FetchError::TooLarge); }
    let title = extract_title(&body);
    Ok(FetchResult { url: screened.to_string(), status, title, text: html_to_text(&body), tier: 1 })
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title>")? + 7;
    let end = lower[start..].find("</title>")? + start;
    Some(html[start..end].trim().to_string())
}

// ponytail: naive tag-strip. Good enough for claim extraction; swap for a real
// readability crate only if extraction quality measurably suffers.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for c in html.chars() {
        match c { '<' => in_tag = true, '>' => in_tag = false, _ if !in_tag => out.push(c), _ => {} }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
```

- [ ] **Step 4: Wire `fetch` into `server.rs` `tools/call`**

Parse `{url, render?}` from params; load `deny_hosts` + timeout from config (Task placeholder: read env `MUR_RESEARCH_DENY_HOSTS` comma-split + `MUR_RESEARCH_TIMEOUT_SECS`, default 20 — replace with config.yaml read in Task 6). Call `fetch_tier1` when `render` is false/absent; return `FetchResult` as JSON. Map `FetchError::Guard` to a JSON-RPC error with a clear message.

- [ ] **Step 5: Run tests + build**

Run: `cargo test -p mur-research-gateway && cargo build -p mur-research-gateway`
Expected: PASS + clean build.

- [ ] **Step 6: Commit**

```bash
git add mur-research-gateway/src/fetcher.rs mur-research-gateway/src/main.rs mur-research-gateway/src/server.rs
git commit -m "feat(research-gateway): tier-1 SSRF-guarded fetch"
```

---

### Task 4: browser tiers (2/3) + `search` + preflight — agent-browser driver

Add the escalation tiers and search, driving `agent-browser` as a subprocess. Pre-spawn `screen_url` (the proxy can't see the browser's own connections — spec §5). Preflight degrades explicitly when the toolchain is missing.

**Files:**
- Create: `mur-research-gateway/src/browser.rs`
- Modify: `mur-research-gateway/src/server.rs` (wire `search`; `fetch` with `render=true` → tier 2/3)

**Interfaces:**
- Consumes: `net_guard::screen_url`, `fetcher::FetchResult`.
- Produces:
  - `struct BrowserCfg { agent_browser_bin: String, lightpanda_path: Option<String>, chrome_stealth_args: String }`
  - `fn preflight(cfg: &BrowserCfg) -> Preflight` where `enum Preflight { Full, LightpandaMissing, AgentBrowserTooOld(String), AgentBrowserMissing }`
  - `async fn fetch_rendered(url: &str, deny: &[String], cfg: &BrowserCfg, want_chrome: bool) -> Result<FetchResult, FetchError>`
  - `async fn search(query: &str, limit: usize, cfg: &BrowserCfg) -> Result<Vec<SearchHit>, FetchError>`; `struct SearchHit { title: String, url: String, snippet: String }`

- [ ] **Step 1: Write the failing tests (command construction + preflight, no real browser)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lightpanda_command_forces_empty_args() {
        let cfg = BrowserCfg { agent_browser_bin: "agent-browser".into(),
            lightpanda_path: Some("/x/lightpanda".into()), chrome_stealth_args: "--no-sandbox".into() };
        let argv = build_fetch_argv("https://example.com", &cfg, false);
        // lightpanda tier MUST pass --args "" and --executable-path, and MUST NOT carry stealth args
        assert!(argv.windows(2).any(|w| w[0] == "--args" && w[1] == ""));
        assert!(argv.windows(2).any(|w| w[0] == "--executable-path" && w[1] == "/x/lightpanda"));
        assert!(argv.windows(2).any(|w| w[0] == "--engine" && w[1] == "lightpanda"));
        assert!(!argv.iter().any(|a| a.contains("no-sandbox")));
    }
    #[test]
    fn chrome_tier_carries_stealth_args() {
        let cfg = BrowserCfg { agent_browser_bin: "agent-browser".into(),
            lightpanda_path: Some("/x/lightpanda".into()), chrome_stealth_args: "--no-sandbox".into() };
        let argv = build_fetch_argv("https://example.com", &cfg, true);
        assert!(argv.windows(2).any(|w| w[0] == "--engine" && w[1] == "chrome"));
        assert!(argv.iter().any(|a| a == "--no-sandbox"));
    }
    #[test]
    fn preflight_degrades_when_lightpanda_missing() {
        let cfg = BrowserCfg { agent_browser_bin: "agent-browser".into(),
            lightpanda_path: None, chrome_stealth_args: String::new() };
        // With no lightpanda path, preflight must not claim Full.
        assert!(!matches!(preflight_from_versions(true, Some("0.31.1"), &cfg), Preflight::Full));
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p mur-research-gateway browser`
Expected: FAIL — `browser` unresolved.

- [ ] **Step 3: Implement `browser.rs`**

```rust
use crate::fetcher::{FetchError, FetchResult};
use crate::net_guard;

pub struct BrowserCfg {
    pub agent_browser_bin: String,
    pub lightpanda_path: Option<String>,
    pub chrome_stealth_args: String, // comma-separated; empty = none
}

pub enum Preflight { Full, LightpandaMissing, AgentBrowserTooOld(String), AgentBrowserMissing }
pub struct SearchHit { pub title: String, pub url: String, pub snippet: String }

/// Build the agent-browser argv for a single fetch. Pure → unit-testable.
/// lightpanda tier: --engine lightpanda --executable-path PATH --args "" (MANDATORY:
/// stealth args must never reach lightpanda). chrome tier: --engine chrome + stealth.
pub fn build_fetch_argv(url: &str, cfg: &BrowserCfg, want_chrome: bool) -> Vec<String> {
    let mut a = Vec::new();
    if want_chrome || cfg.lightpanda_path.is_none() {
        a.push("--engine".into()); a.push("chrome".into());
        for s in cfg.chrome_stealth_args.split(',').filter(|s| !s.is_empty()) { a.push(s.to_string()); }
    } else {
        a.push("--engine".into()); a.push("lightpanda".into());
        a.push("--executable-path".into()); a.push(cfg.lightpanda_path.clone().unwrap());
        a.push("--args".into()); a.push(String::new()); // MANDATORY empty
    }
    a.push("--session".into()); a.push(session_id(url)); // per-fetch isolation
    a.push("open".into()); a.push(url.to_string()); a.push("snapshot".into());
    a
}

fn session_id(url: &str) -> String {
    // ponytail: deterministic per-URL session so concurrent fetches don't share
    // cookie jars; hash keeps it filesystem-safe.
    let mut h: u64 = 1469598103934665603;
    for b in url.bytes() { h ^= b as u64; h = h.wrapping_mul(1099511628211); }
    format!("rg-{h:016x}")
}

pub fn preflight_from_versions(ab_present: bool, ab_version: Option<&str>, cfg: &BrowserCfg) -> Preflight {
    if !ab_present { return Preflight::AgentBrowserMissing; }
    if let Some(v) = ab_version { if !version_ge(v, 0, 28) { return Preflight::AgentBrowserTooOld(v.into()); } }
    if cfg.lightpanda_path.is_none() { return Preflight::LightpandaMissing; }
    Preflight::Full
}

fn version_ge(v: &str, maj: u32, min: u32) -> bool {
    let mut it = v.trim_start_matches('v').split('.');
    let a: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let b: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    a > maj || (a == maj && b >= min)
}

/// Fetch a JS-rendered page. Pre-spawn SSRF/deny screen (proxy can't see the
/// browser's connections — spec §5), then drive agent-browser.
pub async fn fetch_rendered(url: &str, deny: &[String], cfg: &BrowserCfg, want_chrome: bool) -> Result<FetchResult, FetchError> {
    let screened = net_guard::screen_url(url, deny).map_err(FetchError::Guard)?;
    let argv = build_fetch_argv(screened.as_str(), cfg, want_chrome);
    let out = tokio::process::Command::new(&cfg.agent_browser_bin)
        .args(&argv)
        .output().await.map_err(|e| FetchError::Http(format!("spawn agent-browser: {e}")))?;
    if !out.status.success() {
        return Err(FetchError::Http(String::from_utf8_lossy(&out.stderr).into()));
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let tier = if want_chrome || cfg.lightpanda_path.is_none() { 3 } else { 2 };
    Ok(FetchResult { url: screened.to_string(), status: 200, title: None, text, tier })
}
```

Implement `search(query, limit, cfg)` by driving agent-browser to a search-results page (engine per preflight) and parsing hits; keep the parser small and behind the same pre-spawn screen. (Ponytail: v1 search quality rides the upstream engine; a dedicated search API is Out of Scope per spec §11.)

- [ ] **Step 4: Wire `search` + rendered `fetch` into `server.rs`**

`search` → `browser::search`. `fetch` with `render=true` → `fetch_rendered` (chrome only when a caller-supplied `chrome` flag or a tier-2 failure escalates). Load `BrowserCfg` from config/env (Lightpanda path from `MUR_RESEARCH_LIGHTPANDA_PATH`, default to the installed `~/.mur/…/lightpanda`; finalized in Task 6).

- [ ] **Step 5: Run tests + build**

Run: `cargo test -p mur-research-gateway && cargo build -p mur-research-gateway`
Expected: PASS + clean build.

- [ ] **Step 6: Commit**

```bash
git add mur-research-gateway/src/browser.rs mur-research-gateway/src/server.rs
git commit -m "feat(research-gateway): browser tiers 2/3 + search + preflight (pre-spawn SSRF)"
```

---

### Task 5: URL-level audit

Every `search`/`fetch` emits a structured audit record (`worker`, `url`/`query`, `tier`, `outcome`). This is the sole request-level evidence for the browser tiers (proxy is blind to them — spec §7.2/§7.4).

**Files:**
- Create: `mur-research-gateway/src/audit.rs`
- Modify: `mur-research-gateway/src/server.rs` (emit on every call)

**Interfaces:**
- Produces: `fn audit(record: AuditRecord)` emitting a single-line JSON `tracing::info!(target: "research_gateway_audit", ...)`; `struct AuditRecord { worker: Option<String>, verb: &'static str, target: String, tier: Option<u8>, outcome: &'static str }`. `worker` is read from the `MUR_AGENT_NAME` env the runtime sets on the child (fallback `None`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn audit_line_is_single_json_object() {
    let line = super::render_audit(&AuditRecord {
        worker: Some("worker_3".into()), verb: "fetch",
        target: "https://example.com".into(), tier: Some(1), outcome: "ok" });
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["verb"], "fetch");
    assert_eq!(v["tier"], 1);
    assert_eq!(v["outcome"], "ok");
}
```

- [ ] **Step 2: Run to verify fail** — `cargo test -p mur-research-gateway audit` → FAIL.

- [ ] **Step 3: Implement `audit.rs`** — `render_audit(&AuditRecord) -> String` (serde_json), and `audit(rec)` that logs `tracing::info!(target: "research_gateway_audit", "{}", render_audit(&rec))`. Read `worker` from `std::env::var("MUR_AGENT_NAME").ok()`.

- [ ] **Step 4: Emit in `server.rs`** — call `audit(...)` at the end of each `search`/`fetch` branch with the real outcome (`ok` / `denied` / `error`), tier from the result (denied → `tier: None`).

- [ ] **Step 5: Run + build** — `cargo test -p mur-research-gateway && cargo build -p mur-research-gateway` → PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-research-gateway/src/audit.rs mur-research-gateway/src/server.rs mur-research-gateway/src/main.rs
git commit -m "feat(research-gateway): URL-level audit for every search/fetch"
```

---

### Task 6: config + install wiring — ship the gateway binary and read config.yaml

Finalize config (no hardcoded values) and make the binary discoverable/shippable so agent sandboxes can spawn it.

**Files:**
- Create: `mur-research-gateway/src/config.rs`
- Modify: `mur-research-gateway/src/server.rs` (load config once at startup)
- Modify: `build.sh` (install `mur-research-gateway` alongside `mur`)
- Create: `docs/attribution/lightpanda-AGPL.md` (AGPL notice + upstream source link)

**Interfaces:**
- Produces: `struct GatewayConfig { deny_hosts: Vec<String>, timeout: Duration, browser: BrowserCfg, search_limit: usize }`; `fn load(mur_home: &Path) -> GatewayConfig` reading `~/.mur/config.yaml` key `research_gateway:` with documented defaults; env vars override.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn config_defaults_and_env_override() {
    std::env::set_var("MUR_RESEARCH_TIMEOUT_SECS", "45");
    let c = load_from_yaml("", /*mur_home*/ std::path::Path::new("/nonexistent"));
    assert_eq!(c.timeout.as_secs(), 45);        // env override
    assert!(c.search_limit >= 1);                // documented default present
    std::env::remove_var("MUR_RESEARCH_TIMEOUT_SECS");
}
```

- [ ] **Step 2: Run → FAIL** — `cargo test -p mur-research-gateway config`.

- [ ] **Step 3: Implement `config.rs`** — parse the optional `research_gateway:` block (serde_yaml, mirror how other crates read config.yaml); defaults: `timeout=20s`, `search_limit=8`, `chrome_stealth_args` = the documented stealth set, `lightpanda_path` = first existing of env → configured → `~/.mur/browser/lightpanda`. Every default is a named `const`, not an inline literal.

- [ ] **Step 4: Wire startup** — `server::McpServer::new()` loads config once; store on the struct; pass `&config` into fetch/search.

- [ ] **Step 5: Install in `build.sh`** — after the `mur` install line, add `cargo build --release -p mur-research-gateway` and copy `target/release/mur-research-gateway` to the same bin dir as `mur`. Add the AGPL attribution doc.

- [ ] **Step 6: Run + build + install smoke**

Run: `cargo test -p mur-research-gateway && cargo build --release -p mur-research-gateway`
Expected: PASS; the release binary exists.

- [ ] **Step 7: Commit**

```bash
git add mur-research-gateway/src/config.rs mur-research-gateway/src/server.rs build.sh docs/attribution/lightpanda-AGPL.md
git commit -m "feat(research-gateway): config.yaml + env config, ship binary, AGPL attribution"
```

---

### Task 7: worker profile template + provisioning command

Create the restricted worker agents, each mounting only the gateway MCP server (egress still `Inherit` — grant is Task 8, the explicit consent step).

**Files:**
- Create: `mur-core/src/cmd/deep_research/mod.rs` (subcommand module)
- Create: `mur-core/src/cmd/deep_research/provision.rs`
- Modify: `mur-core/src/cmd/mod.rs` + CLI arg enum (register `mur deep-research provision`)
- Test: `mur-core/src/cmd/deep_research/provision.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `mur_common::agent::{AgentProfile, McpServerEntry}`; `cmd_fleet_create`.
- Produces: `fn provision(mur_home, name_prefix: &str, count: usize) -> Result<Vec<String>>` — creates `count` agents `<prefix>_1..<prefix>_N`, each with entitlements `network.outbound = restricted` (empty allow) and one `mcp_servers` entry `{name:"research-gateway", command:"mur-research-gateway", args:[], network: None}`. Returns the created names.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn provision_creates_restricted_workers_with_gateway() {
    let tmp = tempfile::tempdir().unwrap();
    let names = provision(tmp.path(), "dr_worker", 3).unwrap();
    assert_eq!(names.len(), 3);
    let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();
    assert!(p.mcp_servers.iter().any(|s| s.name == "research-gateway"));
    // egress NOT granted here — must be Inherit/restricted until the consent step
    let gw = p.mcp_servers.iter().find(|s| s.name == "research-gateway").unwrap();
    assert!(gw.network.is_none());
}
```

- [ ] **Step 2: Run → FAIL** — `cargo test -p mur-core --bin mur provision` (bin tests need `RUST_MIN_STACK=33554432` — `mem:gotcha_mur_core_bin_nextest_stack_overflow`).

- [ ] **Step 3: Implement `provision.rs`** — loop `1..=count`, build an `AgentProfile` with the restricted entitlement + gateway `McpServerEntry`, write via the existing profile writer (atomic temp+rename). Use the real profile-construction path `mur agent create` uses (call the same helper, don't hand-roll YAML).

- [ ] **Step 4: Run → PASS**, then `cargo build -p mur-core` (needs `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist`).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/deep_research mur-core/src/cmd/mod.rs
git commit -m "feat(deep-research): provision restricted worker agents mounting the gateway"
```

---

### Task 8: egress grant step (explicit consent) in provisioning

Grant `broad-audited` to each worker's gateway server via the shipped consent path — a separate, explicit step so it can never be implicit.

**Files:**
- Modify: `mur-core/src/cmd/deep_research/provision.rs` (add `--grant-egress` flag path)
- Test: same file.

**Interfaces:**
- Consumes: the shipped `cmd_mcp_set_network(agent, "research-gateway", broad_audited=true, deny_hosts, yes)` (from #661, `cmd/agent/mcp.rs`).
- Produces: `fn grant_egress(mur_home, worker: &str, deny_hosts: &[String], yes: bool) -> Result<()>` — sets the gateway server to `BroadAudited` with an `EgressAuthorization`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn grant_sets_broad_audited_with_authorization() {
    let tmp = tempfile::tempdir().unwrap();
    let names = provision(tmp.path(), "dr_worker", 1).unwrap();
    grant_egress(tmp.path(), &names[0], &["evil.example".into()], /*yes=*/true).unwrap();
    let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();
    let gw = p.mcp_servers.iter().find(|s| s.name == "research-gateway").unwrap();
    let net = gw.network.as_ref().unwrap();
    assert!(matches!(net.mode, mur_common::agent::McpNetMode::BroadAudited));
    assert!(net.authorization.is_some());
    assert!(net.deny_hosts.contains(&"evil.example".to_string()));
}
```

- [ ] **Step 2: Run → FAIL** — `RUST_MIN_STACK=33554432 cargo test -p mur-core --bin mur grant_sets_broad`.

- [ ] **Step 3: Implement `grant_egress`** — call the shipped `cmd_mcp_set_network` with `broad_audited=true`; when invoked from `provision --grant-egress`, prompt for consent per worker unless `--yes`. Never call this from fleet create.

- [ ] **Step 4: Run → PASS**, `cargo build -p mur-core`.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/deep_research/provision.rs
git commit -m "feat(deep-research): explicit broad-audited egress grant per worker (consent-gated)"
```

---

### Task 9: fleet-scoped research skills (router + worker prompts)

Reshape the five `aura-*` skills into fleet roles. Content only (YAML); the escalation-ladder skill is retired (now gateway code).

**Files:**
- Create: `~/.mur/skills/…` is runtime data — instead create source templates: `mur-core/src/skills/deep_research_router.yaml`, `deep_research_worker.yaml`, `deep_research_verify.yaml` (mirror where `mur_parallel_exec.yaml` etc. live).
- Modify: skill loader registration if source skills are compiled in (follow the pattern of the existing `*.yaml` in `mur-core/src/skills/`).

**Interfaces:**
- Produces: three skills with `scope: Fleet`, triggers/tags wired so they inject only for `deep-research` members. Router skill states the decompose→assign→synthesize procedure incl. emitting `RESEARCH_COMPLETE` on its own line. Worker skill: `search`/`fetch` via the gateway + citation discipline. Verify skill: adversarial refutation under one lens.

- [ ] **Step 1: Write the failing test** — a loader test asserting the three YAML files parse into `SkillManifest` with `scope: Fleet` and non-empty procedure (mirror the existing skill-loader test).

- [ ] **Step 2: Run → FAIL**.

- [ ] **Step 3: Author the three YAMLs** — reuse the prose from `aura-citation-discipline`, `aura-source-triangulation`, `aura-parallel-fanout`; drop `aura-research-escalation-ladder` (superseded by gateway code) and note the retirement in the worker skill.

- [ ] **Step 4: Run → PASS** (`cargo test -p mur-core --bin mur skills`).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/skills/deep_research_*.yaml
git commit -m "feat(deep-research): fleet-scoped router/worker/verify skills"
```

---

### Task 10: orchestration TDD against a stub gateway

Prove the full loop (decompose→research→verify→synthesize→marker convergence) with NO network, using a stub gateway binary that returns a fixed corpus. This is the load-bearing integration test.

**Files:**
- Create: `mur-research-gateway/src/bin/stub_gateway.rs` (a fixed-corpus MCP server for tests)
- Create: `mur-core/tests/deep_research_loop.rs` (integration test)

**Interfaces:**
- Consumes: `provision`, `cmd_fleet_create`, `mur fleet run --loop` executor entry (`cmd/fleet/loop_run.rs`).
- Produces: an integration test that provisions 2 workers pointed at `stub_gateway`, creates a `deep-research` fleet with `done_when: marker:RESEARCH_COMPLETE`, runs one bounded loop, and asserts a synthesized report with ≥1 cited claim appears in the channel.

- [ ] **Step 1: Write `stub_gateway.rs`** — same MCP handshake as the real one; `search` returns 2 fixed hits, `fetch` returns fixed text containing a known fact + a citable URL. Deterministic.

- [ ] **Step 2: Write the failing integration test**

```rust
#[tokio::test]
async fn deep_research_loop_converges_with_stub_gateway() {
    // provision 2 workers whose gateway command = the built stub_gateway binary
    // create fleet "dr-test", router=mur, done_when marker:RESEARCH_COMPLETE, budget small
    // run_loop with max_iterations bounded
    // assert: channel contains a synthesis event with a citation URL from the stub corpus
}
```

- [ ] **Step 3: Run → FAIL** (`cargo test -p mur-core --test deep_research_loop`).

- [ ] **Step 4: Implement the wiring** — a thin `mur deep-research run <fleet>` that calls the existing loop executor; make the test drive it. Keep new code minimal — reuse `loop_run`.

- [ ] **Step 5: Run → PASS**.

- [ ] **Step 6: Commit**

```bash
git add mur-research-gateway/src/bin/stub_gateway.rs mur-core/tests/deep_research_loop.rs mur-core/src/cmd/deep_research
git commit -m "test(deep-research): full loop converges against stub gateway (no network)"
```

---

### Task 11: real E2E — one small question

Swap in the real gateway with a real broad-audited grant; run one narrow question end-to-end; verify cited report + per-claim signed-channel provenance + audit reconciliation.

**Files:**
- Create: `docs/superpowers/plans/artifacts/mur-native-deep-research-e2e.md` (record the run + evidence)

- [ ] **Step 1:** `MUR_WEB_DIST=… ORT_STRATEGY=download ./build.sh --install` (ships `mur` + `mur-research-gateway`).
- [ ] **Step 2:** `mur deep-research provision --count 4 --grant-egress` (consent per worker).
- [ ] **Step 3:** `mur fleet create deep-research --members dr_worker_1,dr_worker_2,dr_worker_3,dr_worker_4 --router mur --goal "<a narrow, verifiable question>"`; set `done_when: marker:RESEARCH_COMPLETE` + a small `--budget-usd`.
- [ ] **Step 4:** `MUR_FLEET_AUTORUN=0 mur fleet run deep-research --loop --budget-usd 3 --deadline 30m`.
- [ ] **Step 5: Verify (evidence before claims)** — the channel holds a synthesized cited report; every citation reconciles to a `research_gateway_audit` line; claims are signed by their producing worker (per-actor `channel_verify`). Record all of it in the artifact doc. If any citation lacks an audit line, STOP — that's an egress-bypass bug.
- [ ] **Step 6: Commit** the artifact.

---

## Notes for the implementer

- **Build the gateway crate first and in isolation** — it has no ORT/web-dist deps, so `cargo build -p mur-research-gateway` is fast. The `mur-core` tasks (7–10) need `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist` and the rustup toolchain on PATH, and are SLOW to compile cold (~20min) — build FOREGROUND, don't background+poll. — `mem:project_agent_egress_governance`.
- **bin tests** (`cmd/*`) run under `cargo test -p mur-core --bin mur` with `RUST_MIN_STACK=33554432`. — `mem:gotcha_mur_core_bin_nextest_stack_overflow`.
- **Never grant egress from fleet create** — only the explicit `--grant-egress` consent step (Task 8). — spec §7.1.
- **agent-browser preflight**: `npm i -g agent-browser@latest` (>=0.28.0); Lightpanda binary is already installed (`~/.mur/…/lightpanda`, verified 2026-07-08); `pkill -f agent-browser` if the daemon is stale. — `mem:gotcha_agent_browser_lightpanda_engine_dead`.
