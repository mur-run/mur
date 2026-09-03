# Plan — MUR Hub Claude Subscription provider

> **Execute with `mur-executing-plans`.** Work task-by-task in order. Task 1
> is in `/Volumes/Firecuda4tb/Projects/mur-model-gateway` (use a worktree on a
> fresh branch from `main`); Tasks 2–8 are in the `mur` repository on branch
> `feat/claude-subscription` (worktree `.worktrees/claude-subscription`). Do
> not begin a later task until the preceding task's tests and review gate
> pass.

Design: `docs/superpowers/specs/2026-09-03-mur-hub-claude-subscription-design.md`.
Reference implementation (everything here mirrors it): the ChatGPT provider,
mur#1154 and mur-model-gateway#11, design
`docs/superpowers/specs/2026-09-02-mur-hub-chatgpt-subscription-design.md`.

## Goal

Let MUR Hub connect a Claude Pro/Max subscription through Claude Code's
login, with a registry provider that can only ever reach the loopback
gateway, so no edit can silently move an agent onto API billing.

## Architecture

The gateway already attaches the Claude Code OAuth token to authless
`/v1/messages` requests; this plan adds the control plane and the safety
property. Runtime gains `provider: claude` (authless, loopback-only `/v1`).
Hub gains a `claude_subscription` module that wraps `claude auth
status/login/logout`, lists models from the models.dev catalog, and reuses
the ChatGPT gateway lifecycle and registry code. The ChatGPT panel becomes a
descriptor-driven `SubscriptionProviderPanel` serving both providers.

## Tech stack

Rust 2024 (`mur-common`, `mur-agent-runtime`, `mur-core`, Tauri 2, Axum),
React 18 + TypeScript + Vitest, YAML model registry, the Rust
`mur-model-gateway` service, Claude Code CLI ≥ 2.1.258 (`claude auth`).

## Global Constraints

Copied from the approved design. Every task implicitly includes all of them.

- Claude Code owns authentication and credential storage (OS keychain or
  `~/.claude/.credentials.json`).
- MUR Hub owns the connection experience, account/model status, and model registration.
- `claude auth status --json` is the account control plane; only `loggedIn && authMethod == "claude.ai"` is a subscription login. Any other value is "signed in, but not this provider".
- `mur-model-gateway` remains the inference data plane; its forwarding code is not modified.
- The runtime sends Anthropic Messages requests with **no `x-api-key` header at all** (absent, never empty) and only to `http://<localhost|loopback-ip>:<port>/v1`.
- Hub and runtime never read, parse, log, or serialize the Claude Code credential (keychain blob or credentials file).
- No UI, command result, diagnostic, or registry field contains a token; `claude auth status` output beyond `loggedIn`, `authMethod`, `email` is discarded before it reaches any view or log.
- `provider` is exactly `claude`; `secret` is absent.
- Existing `provider: anthropic` behavior — including entries already pointed at the gateway — remains unchanged.
- MUR never inserts a usage-billed model into a subscription model's fallback chain automatically; a 429 grants no permission to add one.
- Disconnecting MUR does not modify the shared Claude Code login; signing out requires confirmation because it affects every Claude Code session and IDE extension.
- Registry discovery/add is non-destructive: an existing alias always wins.
- `mur model doctor` only warns; it never rewrites an entry and never changes the exit code.
- Repo rules: no hardcoded secrets, single source file ≤ 800 lines (files already over the limit — `anthropic.rs`, `lib.rs` in the gateway — may grow by the lines this plan adds, nothing more), `cargo fmt` clean, `cargo clippy --all-targets -- -D warnings` clean, UI strings in both English and Traditional Chinese.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `/Volumes/Firecuda4tb/Projects/mur-model-gateway/src/lib.rs` | `claudeCredential` in `/__mur/health` | 1 |
| `/Volumes/Firecuda4tb/Projects/mur-model-gateway/tests/health.rs` | health contract for the Claude credential kind | 1 |
| `mur-agent-runtime/src/llm/anthropic.rs` | explicit `AnthropicAuth`; authless constructor | 2 |
| `mur-agent-runtime/src/llm/loopback.rs` | shared loopback-URL validator (path-parameterized) | 3 |
| `mur-agent-runtime/src/llm/codex.rs` | delegate to the shared validator | 3 |
| `mur-agent-runtime/src/llm/claude.rs` | loopback-only authless `ClaudeClient` | 3 |
| `mur-agent-runtime/src/llm/mod.rs` | export `claude`, `loopback` | 3 |
| `mur-agent-runtime/src/llm/client_builder.rs` | `"claude"` factory arm | 3 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/process.rs` | `run_bounded` shared; `claudeCredential` parsed into the gateway view | 4 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/registry.rs` | provider-parameterized add/disconnect | 4 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/mod.rs` | `Subscription*View` aliases; `FAKE_BIN_LOCK` shared | 4 |
| `mur-hub-gui/src-tauri/src/claude_subscription/mod.rs` | views, `claude` resolution, Tauri commands | 5 |
| `mur-hub-gui/src-tauri/src/claude_subscription/account.rs` | `claude auth status/login/logout` | 5 |
| `mur-hub-gui/src-tauri/src/claude_subscription/catalog.rs` | model list from models.dev | 5 |
| `mur-hub-gui/src-tauri/src/lib.rs` | module + command registration | 5 |
| `mur-hub-gui/ui/src/components/chatgptSubscription.ts` | readiness-parameterized state machine | 6 |
| `mur-hub-gui/ui/src/components/chatgptSubscription.test.ts` | tests for both readiness rules | 6 |
| `mur-hub-gui/ui/src/components/modelLibraryHelpers.ts` | `SubscriptionDescriptor`, `CHATGPT_SUBSCRIPTION`, `CLAUDE_SUBSCRIPTION` | 6 |
| `mur-hub-gui/ui/src/components/SubscriptionProviderPanel.tsx` | renamed, descriptor-driven panel | 6 |
| `mur-hub-gui/ui/src/components/ModelLibrary.tsx` | rail + routing for every subscription descriptor | 6 |
| `mur-hub-gui/ui/src/i18n/en.ts`, `zh-TW.ts` | `lib.claude.*` copy; `loggedOutApiBilled` for both | 6 |
| `mur-core/src/cmd/model_doctor.rs` | two warn-only subscription checks | 7 |
| `docs/model-gateway.md`, `README.md` | user-facing docs | 7 |

---

## Task 1 — Report the Claude credential kind in gateway health

**Repository:** `/Volumes/Firecuda4tb/Projects/mur-model-gateway`. Create a
worktree: `git worktree add -b feat/claude-health /private/tmp/mur-model-gateway-claude origin/main`.

**Interfaces**

Consumes (existing):

```rust
impl TokenSource {
    pub fn resolve_credential(&self) -> Result<Option<keychain::OauthCredential>, keychain::KeychainError>;
}
// TokenSource::CredentialsFile(PathBuf) reads {"claudeAiOauth":{"accessToken":…}}
```

Produces:

```text
GET /__mur/health
// 200 JSON now also carries:
// "claudeCredential": "oauth" | "missing"
```

`oauth` means a Claude Code OAuth credential is readable from the configured
Anthropic token source; `missing` covers every other case (no blob, backend
error, `Disabled`, `Static`, `EnvVar`). Never the token, never the expiry.

**Steps**

- [x] In `tests/health.rs`, change the `spawn` helper to take both token
  sources and add the Claude assertions. Replace the whole file with:

```rust
//! `/__mur/health`: a loopback readiness probe MUR Hub polls before routing a
//! subscription agent through the gateway. It reports *which kind* of
//! credential is on disk — never the credential itself.

use mur_model_gateway::{AppState, TokenSource, build_router};
use std::io::Write;

async fn spawn(anthropic: TokenSource, codex: TokenSource) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let dead = "http://127.0.0.1:9";
    let state = AppState::new(dead, dead, dead, anthropic)
        .unwrap()
        .with_token_source_codex(codex);
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    format!("http://{addr}/__mur/health")
}

#[tokio::test]
async fn health_is_local_and_non_secret() {
    let url = spawn(TokenSource::Disabled, TokenSource::Disabled).await;
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), 200);
    let raw = resp.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["status"], "ok");
    assert!(json.get("codexHook").unwrap().is_boolean());
    assert_eq!(json["codexCredential"], "missing");
    assert_eq!(json["claudeCredential"], "missing");
    assert!(json.get("compression").unwrap().is_boolean());
    assert!(!raw.contains("access_token"));
    assert!(!raw.contains("refresh_token"));
    assert!(!raw.contains("accessToken"));
}

#[tokio::test]
async fn health_reports_credential_mode_without_the_credential() {
    for (blob, mode) in [
        (
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"at-SECRET","refresh_token":"rt-SECRET","account_id":"acct-SECRET"}}"#,
            "chatgpt",
        ),
        (r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-SECRET"}"#, "apikey"),
        (r#"{"auth_mode":"apikey"}"#, "missing"),
        ("not json", "missing"),
    ] {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(blob.as_bytes()).unwrap();
        let url = spawn(
            TokenSource::Disabled,
            TokenSource::Codex(f.path().to_path_buf()),
        )
        .await;
        let raw = reqwest::get(&url).await.unwrap().text().await.unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(json["codexCredential"], mode, "{blob}");
        assert!(!raw.contains("SECRET"), "{raw}");
    }
}

#[tokio::test]
async fn health_reports_claude_credential_kind_without_the_credential() {
    for (blob, mode) in [
        (
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-SECRET","refreshToken":"rt-SECRET","expiresAt":1787497765291}}"#,
            "oauth",
        ),
        (r#"{"claudeAiOauth":{}}"#, "missing"),
        ("not json", "missing"),
    ] {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(blob.as_bytes()).unwrap();
        let url = spawn(
            TokenSource::CredentialsFile(f.path().to_path_buf()),
            TokenSource::Disabled,
        )
        .await;
        let raw = reqwest::get(&url).await.unwrap().text().await.unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(json["claudeCredential"], mode, "{blob}");
        assert!(!raw.contains("SECRET"), "{raw}");
        assert!(!raw.contains("sk-ant"), "{raw}");
        assert!(!raw.contains("1787497765291"), "expiry leaked: {raw}");
    }
    // A missing file is `missing`, not an error.
    let url = spawn(
        TokenSource::CredentialsFile("/nonexistent/credentials.json".into()),
        TokenSource::Disabled,
    )
    .await;
    let json: serde_json::Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
    assert_eq!(json["claudeCredential"], "missing");
}
```

- [x] Run `cargo test --test health` and watch the two Claude assertions fail
  (`claudeCredential` is null).

- [x] In `src/lib.rs`, replace the `health` handler body with:

```rust
async fn health(State(state): State<AppState>) -> axum::Json<serde_json::Value> {
    let codex = match &state.token_source_codex {
        TokenSource::Codex(path) => match codex::read_credential(path) {
            Some(codex::CodexCredential::OAuth { .. }) => "chatgpt",
            Some(codex::CodexCredential::ApiKey { .. }) => "apikey",
            None => "missing",
        },
        _ => "missing",
    };
    // Same memoised read `forward()` uses (keychain::CACHE_TTL), so polling
    // health adds no keychain traffic. A kind, never a token or expiry.
    let claude = match state.token_source.resolve_credential() {
        Ok(Some(_)) => "oauth",
        _ => "missing",
    };
    axum::Json(serde_json::json!({
        "status": "ok",
        "codexHook": codex::hook_compiled(),
        "codexCredential": codex,
        "claudeCredential": claude,
        "compression": state.compress,
    }))
}
```

  Update the doc comment above it to: `/// Loopback readiness for MUR Hub.
  /// \`codexCredential\` is exactly \`chatgpt\` / \`apikey\` / \`missing\`,
  /// \`claudeCredential\` exactly \`oauth\` / \`missing\` — kinds, never ids,
  /// tokens, or expiries.`

- [x] Run `cargo test --test health` and expect three passing tests. Run
  `cargo test`, `cargo fmt --check`, and
  `cargo clippy --all-targets -- -D warnings`; expect zero failures/warnings.

- [x] Commit in the gateway worktree: `git add src/lib.rs tests/health.rs`,
  then `git commit -m "feat(health): report the Claude credential kind"`.
  Push and open a PR against `main`; merging it is a prerequisite for the
  acceptance run in Task 8, not for Tasks 2–7.

---

## Task 2 — Make Anthropic transport authentication explicit

**Interfaces**

Consumes: existing `AnthropicClient` request/response conversion.

Produces:

```rust
#[derive(Clone)]
enum AnthropicAuth { ApiKey(String), None }

impl AnthropicClient {
    pub(crate) fn authless_with_http(base_url: String, model: String, http: reqwest::Client) -> Self;
}
```

All existing public constructors continue to create `AnthropicAuth::ApiKey`.
`AnthropicAuth::None` sends **no** `x-api-key` header (absent, never empty).

**Steps**

- [x] Append these tests to the end of `mod tests` in
  `mur-agent-runtime/src/llm/anthropic.rs` (before the module's closing
  brace):

```rust
    fn ok_message() -> serde_json::Value {
        json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })
    }

    fn hello() -> LlmRequest {
        LlmRequest {
            messages: vec![RichMessage::Text {
                role: "user".into(),
                content: "hi".into(),
            }],
            ..Default::default()
        }
    }

    /// The authless constructor sends no `x-api-key` and no `Authorization`
    /// at all. The gateway picks its mode by header *presence*: an absent
    /// header means "attach the keychain token", an empty one means
    /// "pass through untouched" — and a 401 from Anthropic.
    #[tokio::test]
    async fn authless_client_sends_no_credential_header() {
        let server = httpmock::MockServer::start_async().await;
        let m = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/v1/messages")
                    .matches(|req| {
                        !req.headers.as_ref().is_some_and(|h| {
                            h.iter().any(|(k, _)| {
                                k.eq_ignore_ascii_case("x-api-key")
                                    || k.eq_ignore_ascii_case("authorization")
                            })
                        })
                    });
                then.status(200).json_body(ok_message());
            })
            .await;
        let client = AnthropicClient::authless_with_http(
            server.base_url(),
            "claude-opus-5".into(),
            reqwest::Client::new(),
        );
        let resp = client.generate(hello()).await.unwrap();
        assert_eq!(resp.text, "hi");
        m.assert_async().await;
    }

    /// Existing constructors are unchanged: `new` still sends the key.
    #[tokio::test]
    async fn keyed_client_still_sends_x_api_key() {
        let server = httpmock::MockServer::start_async().await;
        let m = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/v1/messages")
                    .header("x-api-key", "test-key");
                then.status(200).json_body(ok_message());
            })
            .await;
        let client = AnthropicClient::new(server.base_url(), "test-key".into(), "claude-opus-5".into());
        client.generate(hello()).await.unwrap();
        m.assert_async().await;
    }
```

  If `LlmRequest` is not already imported in the test module, add
  `use crate::llm::LlmRequest;` next to the existing `use crate::llm::{…}`.

- [x] Run
  `cargo test -p mur-agent-runtime --lib llm::anthropic::tests::authless_client_sends_no_credential_header`
  and watch it fail to compile (`authless_with_http` absent).

- [x] In `mur-agent-runtime/src/llm/anthropic.rs`, above `pub struct AnthropicClient`, add:

```rust
/// How a request authenticates. Explicit so an authless route is a
/// deliberate choice at construction, not an empty key that happens to be
/// sent as `x-api-key: `. `None` exists for the loopback gateway route
/// (`provider: claude`), where the gateway attaches the Claude Code OAuth
/// token itself — and picks that mode by the header being *absent*.
#[derive(Clone)]
enum AnthropicAuth {
    ApiKey(String),
    None,
}
```

  Change the struct field `api_key: String,` to `auth: AnthropicAuth,`. In
  `new` and `new_with_http_client` replace `api_key,` in the struct literal
  with `auth: AnthropicAuth::ApiKey(api_key),`. Then add, directly after
  `new_with_http_client`:

```rust
    /// Messages transport that sends no credential at all. Only the loopback
    /// gateway route may use this (see `llm::claude`), which is why it is
    /// crate-private: the gateway owns the OAuth token, and a key here would
    /// either leak or silently switch the bill to the Anthropic API.
    pub(crate) fn authless_with_http(base_url: String, model: String, http: reqwest::Client) -> Self {
        Self {
            base_url,
            auth: AnthropicAuth::None,
            version: DEFAULT_VERSION.to_string(),
            model,
            http,
        }
    }

    /// Absent, never empty: the gateway keys its mode on header presence.
    fn apply_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            AnthropicAuth::ApiKey(key) => request.header("x-api-key", key),
            AnthropicAuth::None => request,
        }
    }
```

- [x] Replace both request builders (in `generate` and `generate_stream`).
  The current shape is:

```rust
        let resp = self
            .http
            .post(url)
            .header("anthropic-version", &self.version)
            .header("content-type", "application/json")
            .header("x-api-key", &self.api_key)
            .json(&body)
```

  Change each to:

```rust
        let resp = self
            .apply_auth(
                self.http
                    .post(url)
                    .header("anthropic-version", &self.version)
                    .header("content-type", "application/json"),
            )
            .json(&body)
```

  (`let mut resp = …` in `generate_stream` keeps its `mut`.) Confirm with
  `grep -n 'self.api_key' mur-agent-runtime/src/llm/anthropic.rs` that no
  reference remains.

- [x] Run `cargo test -p mur-agent-runtime --lib llm::anthropic`, then
  `cargo fmt -p mur-agent-runtime` and
  `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings`; expect
  zero failures/warnings.

- [x] Commit: `git add mur-agent-runtime/src/llm/anthropic.rs`, then
  `git commit -m "refactor(anthropic): make transport auth explicit"`.

---

## Task 3 — Shared loopback validator, `ClaudeClient`, factory arm

**Interfaces**

Consumes:

```rust
AnthropicClient::authless_with_http(base_url, model, http)   // Task 2
ModelEntry { provider, model, base_url, secret, .. }
```

Produces:

```rust
// mur-agent-runtime/src/llm/loopback.rs
pub fn validate_loopback_base_url(raw: &str, required_path: &str) -> Result<reqwest::Url, LlmError>;

// mur-agent-runtime/src/llm/codex.rs (unchanged signature, now a wrapper)
pub fn validate_codex_base_url(raw: &str) -> Result<reqwest::Url, LlmError>;

// mur-agent-runtime/src/llm/claude.rs
pub const CLAUDE_ROUTE_PATH: &str = "/v1";
pub struct ClaudeClient { inner: AnthropicClient }
impl ClaudeClient {
    pub fn with_http_client(base_url: String, model: String, http: reqwest::Client) -> Result<Self, LlmError>;
    pub(crate) fn from_entry(entry: &ModelEntry, http: reqwest::Client) -> Result<Self, LlmError>;
}
```

**Steps**

- [x] Create `mur-agent-runtime/src/llm/loopback.rs`:

```rust
//! The one URL shape a subscription provider may dial: this machine's
//! gateway, over plain HTTP, on an explicit port, at exactly the route that
//! provider is for. Shared by `codex` (`/codex/v1`) and `claude` (`/v1`).
//! The loopback restriction is a safety property — an authless request to a
//! remote host is either a request to a stranger or a route that lands on
//! metered billing — so it is enforced here, not merely validated in the Hub.

use super::LlmError;

pub fn validate_loopback_base_url(raw: &str, required_path: &str) -> Result<reqwest::Url, LlmError> {
    let bad = |why: &str| LlmError::Http(format!("base_url {raw:?} rejected: {why}"));
    let url = reqwest::Url::parse(raw).map_err(|e| bad(&e.to_string()))?;
    if url.scheme() != "http" {
        return Err(bad("scheme must be http (loopback only)"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("credentials in the URL are not allowed"));
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if !loopback {
        return Err(bad("host must be localhost or a loopback IP"));
    }
    if url.port().is_none() {
        return Err(bad("an explicit port is required"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(bad("query and fragment are not allowed"));
    }
    if url.path().trim_end_matches('/') != required_path {
        return Err(bad(&format!("path must be exactly {required_path}")));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_required_path_is_the_only_thing_that_differs_between_providers() {
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1", "/v1").is_ok());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1/", "/v1").is_ok());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/codex/v1", "/v1").is_err());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1", "/codex/v1").is_err());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1/messages", "/v1").is_err());
    }
}
```

- [x] In `mur-agent-runtime/src/llm/codex.rs`, replace the body of
  `validate_codex_base_url` (keep its signature and doc comment) with:

```rust
pub fn validate_codex_base_url(raw: &str) -> Result<reqwest::Url, LlmError> {
    super::loopback::validate_loopback_base_url(raw, CODEX_ROUTE_PATH)
}
```

  Remove the now-unused `bad` closure and `url::Host` logic from that file.
  The existing table test `accepts_only_loopback_codex_base_urls` must still
  pass unchanged.

- [x] Create `mur-agent-runtime/src/llm/claude.rs`:

```rust
//! Claude-subscription provider (`provider: claude`).
//!
//! Authless Anthropic Messages traffic to the loopback `mur-model-gateway`,
//! which holds the Claude Code OAuth token and attaches it. The runtime
//! never sees a credential — there is no `secret`, and neither
//! `ANTHROPIC_API_KEY` nor the agent keychain is consulted — so the only
//! thing this module has to get right is *where* the traffic may go. That
//! is what separates it from `provider: anthropic` pointed at the same
//! port: one `base_url` edit there lands on API billing; here it is refused
//! at startup.

use super::anthropic::AnthropicClient;
use super::loopback::validate_loopback_base_url;
use super::{LlmClient, LlmError, LlmRequest, LlmResponse, StreamDelta};
use async_trait::async_trait;
use mur_common::model::ModelEntry;

/// The gateway's Anthropic route. `/v1/messages` is appended by the client.
pub const CLAUDE_ROUTE_PATH: &str = "/v1";

pub struct ClaudeClient {
    inner: AnthropicClient,
}

impl ClaudeClient {
    pub fn with_http_client(
        base_url: String,
        model: String,
        http: reqwest::Client,
    ) -> Result<Self, LlmError> {
        let url = validate_loopback_base_url(&base_url, CLAUDE_ROUTE_PATH)?;
        Ok(Self {
            inner: AnthropicClient::authless_with_http(url.to_string(), model, http),
        })
    }

    /// Registry entry → client. Rejects a `secret` outright rather than
    /// ignoring it: a key on a claude entry means someone expects it to be
    /// sent, and this route never sends one.
    pub(crate) fn from_entry(entry: &ModelEntry, http: reqwest::Client) -> Result<Self, LlmError> {
        if entry.secret.is_some() {
            return Err(LlmError::Http(
                "claude entries take no secret: the loopback gateway holds the Claude Code login"
                    .into(),
            ));
        }
        let base = entry.base_url.as_deref().ok_or_else(|| {
            LlmError::Http("claude entry needs base_url (http://127.0.0.1:<port>/v1)".into())
        })?;
        Self::with_http_client(base.to_string(), entry.model.clone(), http)
    }
}

#[async_trait]
impl LlmClient for ClaudeClient {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.inner.generate(req).await
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        self.inner.generate_stream(req, sink).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::secret::SecretRef;

    #[test]
    fn accepts_only_loopback_v1_base_urls() {
        for ok in [
            "http://127.0.0.1:8088/v1",
            "http://localhost:8088/v1",
            "http://[::1]:8088/v1",
            "http://127.0.0.1:8088/v1/",
        ] {
            assert!(validate_loopback_base_url(ok, CLAUDE_ROUTE_PATH).is_ok(), "{ok}");
        }
        for bad in [
            "https://api.anthropic.com/v1",
            "https://127.0.0.1:8088/v1",
            "http://127.0.0.1:8088",
            "http://127.0.0.1:8088/codex/v1",
            "http://127.0.0.1/v1",
            "http://localhost.evil.test:8088/v1",
            "http://user@127.0.0.1:8088/v1",
            "http://192.168.1.2:8088/v1",
            "http://127.0.0.1:8088/v1?x=1",
            "not a url",
        ] {
            assert!(validate_loopback_base_url(bad, CLAUDE_ROUTE_PATH).is_err(), "{bad}");
        }
    }

    fn entry(base_url: Option<&str>, secret: Option<SecretRef>) -> ModelEntry {
        ModelEntry {
            provider: "claude".into(),
            model: "claude-opus-5".into(),
            base_url: base_url.map(Into::into),
            secret,
            ..Default::default()
        }
    }

    #[test]
    fn factory_builds_only_secret_free_loopback_entries() {
        let http = reqwest::Client::new();
        let ok = ClaudeClient::from_entry(&entry(Some("http://127.0.0.1:8088/v1"), None), http.clone())
            .unwrap();
        assert_eq!(ok.model_name(), "claude-opus-5");

        let missing_url = ClaudeClient::from_entry(&entry(None, None), http.clone());
        assert!(missing_url.err().unwrap().to_string().contains("base_url"));

        let with_secret = ClaudeClient::from_entry(
            &entry(
                Some("http://127.0.0.1:8088/v1"),
                Some(SecretRef::Env("ANTHROPIC_API_KEY".into())),
            ),
            http.clone(),
        );
        assert!(with_secret.err().unwrap().to_string().contains("no secret"));

        let remote = ClaudeClient::from_entry(&entry(Some("https://api.anthropic.com/v1"), None), http);
        assert!(remote.err().unwrap().to_string().contains("rejected"));
    }
}
```

- [x] In `mur-agent-runtime/src/llm/mod.rs`, after `pub mod codex;` add
  `pub mod claude;` and `pub mod loopback;` (keep the list alphabetical:
  `anthropic`, `client_builder`, `claude`, `codex`, `fallback`, `loopback`,
  `ollama`, `openai`, `stub`, `switchable`).

- [x] In `mur-agent-runtime/src/llm/client_builder.rs`, extend the import:

```rust
use crate::llm::{
    anthropic::AnthropicClient, claude::ClaudeClient, codex::CodexClient, ollama::OllamaClient,
    openai::OpenAiClient,
};
```

  and add the arm directly before the existing `"codex"` arm:

```rust
        // Claude subscription via the loopback gateway: no secret is
        // resolved, no env var or keychain is consulted — `from_entry`
        // refuses anything but a secret-free loopback `/v1` URL.
        "claude" => Ok(Arc::new(ClaudeClient::from_entry(entry, guarded_http)?)),
```

- [x] Every test that starts an `httpmock::MockServer` must hold
  `crate::llm::MOCK_SERVER_LOCK` (a `#[cfg(test)] pub(crate) static
  tokio::sync::Mutex<()>` at the end of `llm/mod.rs`) for its whole body —
  including the two added in Task 2. httpmock 0.7 recycles a small pool of
  servers behind one shared runtime, and several `#[tokio::test]`s driving
  their own server at once fail each other's connections (`Connect`,
  refused — not a mock mismatch). Serial they all pass; the lock keeps that
  without `--test-threads=1` for the crate.

- [x] Run `cargo test -p mur-agent-runtime --lib llm::` three times (expect
  the `loopback`, `claude`, and unchanged `codex`/`openai` tests to pass
  every time — the failure above is parallelism-dependent), then
  `cargo fmt -p mur-agent-runtime` and
  `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings`; expect
  zero failures/warnings.

- [x] Commit: `git add mur-agent-runtime/src/llm/loopback.rs
  mur-agent-runtime/src/llm/claude.rs mur-agent-runtime/src/llm/codex.rs
  mur-agent-runtime/src/llm/mod.rs mur-agent-runtime/src/llm/client_builder.rs`,
  `mur-agent-runtime/src/llm/openai.rs` (lock in its two mock tests), then
  `git commit -m "feat(runtime): add loopback Claude subscription provider"`.

---

## Task 4 — Generalize the Hub's shared subscription plumbing

**Interfaces**

Consumes (existing, `mur-hub-gui/src-tauri/src/chatgpt_subscription/`):

```rust
process::run_bounded(cmd: Command, timeout: Duration) -> Result<(bool, String), String>  // private today
process::Health { codex_hook, credential, compression }                                // private
process::GatewayStatusView { installed, running, codex_hook, credential_mode, compression }
registry::{ChatGptModelPick, chatgpt_entry, add_chatgpt_models, disconnect_chatgpt}
ChatGptAccountView, ChatGptModelView
```

Produces:

```rust
// chatgpt_subscription/mod.rs
pub type SubscriptionAccountView = ChatGptAccountView;
pub type SubscriptionModelView = ChatGptModelView;
pub type SubscriptionModelPick = registry::ChatGptModelPick;

// chatgpt_subscription/process.rs
pub(crate) async fn run_bounded(cmd: Command, timeout: Duration) -> Result<(bool, String), String>;
pub struct GatewayStatusView {
    pub installed: bool,
    pub running: bool,
    pub codex_hook: bool,
    pub credential_mode: Option<String>,        // codexCredential
    pub claude_credential_mode: Option<String>, // claudeCredential (None on a pre-Task-1 gateway)
    pub compression: bool,
}

// chatgpt_subscription/registry.rs
pub fn subscription_entry(provider: &str, base_url: &str, pick: &ChatGptModelPick) -> ModelEntry;
pub fn add_subscription_models(reg: &mut ModelRegistry, provider: &str, base_url: &str, picks: &[ChatGptModelPick]) -> Result<u32, String>;
pub fn disconnect_subscription(reg: &mut ModelRegistry, provider: &str) -> u32;
```

`chatgpt_models_add` / `chatgpt_disconnect` keep their names and behaviour
and become one-line callers with `"codex"` / `CHATGPT_GATEWAY_BASE`.

**Steps**

- [x] In `process.rs` tests, extend `health_is_parsed_strictly`: replace the
  `ok` assertion block with:

```rust
        let ok = serde_json::json!({"status":"ok","codexHook":true,"codexCredential":"apikey","compression":false});
        assert_eq!(
            parse_health(&ok),
            Some(Health {
                codex_hook: true,
                credential: "apikey".into(),
                claude_credential: None,
                compression: false
            })
        );
        let with_claude = serde_json::json!({"status":"ok","codexHook":true,"codexCredential":"missing","claudeCredential":"oauth","compression":false});
        assert_eq!(parse_health(&with_claude).unwrap().claude_credential.as_deref(), Some("oauth"));
        let bad_claude = serde_json::json!({"status":"ok","codexHook":true,"codexCredential":"missing","claudeCredential":"sk-ant-oat01-x","compression":false});
        assert_eq!(parse_health(&bad_claude), None, "an unknown claude kind is not a gateway we understand");
```

- [x] Run
  `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml health_is_parsed_strictly`
  and watch it fail to compile (`claude_credential` absent).

- [x] In `process.rs`:
  - change `async fn run_bounded(` to `pub(crate) async fn run_bounded(`;
  - add `claude_credential: Option<String>,` to `struct Health` (after
    `credential`) and `pub claude_credential_mode: Option<String>,` to
    `GatewayStatusView` (after `credential_mode`);
  - in `parse_health`, after the `credential` check add:

```rust
    // Absent on a gateway older than mur-model-gateway#<Task 1 PR>; present
    // means it must be one of the two documented kinds.
    let claude_credential = match v.get("claudeCredential") {
        None => None,
        Some(c) => {
            let c = c.as_str()?;
            if !matches!(c, "oauth" | "missing") {
                return None;
            }
            Some(c.to_string())
        }
    };
```

    and include `claude_credential,` in the returned `Health`;
  - in `status_at`, the `Some(h)` arm gains `claude_credential_mode: h.claude_credential,`.

- [x] In `mod.rs` add, after the `pub use` lines:

```rust
/// The Claude provider reuses these views verbatim (its `plan_type` is
/// always `None`, its models carry no default marker). Type aliases rather
/// than a rename so the UI DTOs and the Task 5–7 commands of mur#1154 keep
/// their field names.
pub type SubscriptionAccountView = ChatGptAccountView;
pub type SubscriptionModelView = ChatGptModelView;
pub type SubscriptionModelPick = registry::ChatGptModelPick;
```

- [x] In `registry.rs`, replace `chatgpt_entry`, `add_chatgpt_models`, and
  `disconnect_chatgpt` with the provider-parameterized forms plus thin
  ChatGPT wrappers:

```rust
pub fn subscription_entry(provider: &str, base_url: &str, pick: &ChatGptModelPick) -> ModelEntry {
    ModelEntry {
        provider: provider.into(),
        model: pick.model.clone(),
        base_url: Some(base_url.into()),
        secret: None,
        tier: Some(RouteTier::Frontier),
        billing: Some(BillingMode::Subscription),
        catalog_verified: Some(pick.verified),
        ..Default::default()
    }
}

pub fn chatgpt_entry(pick: &ChatGptModelPick) -> ModelEntry {
    subscription_entry("codex", CHATGPT_GATEWAY_BASE, pick)
}

/// Validate every pick first, then insert; an existing alias always wins,
/// whatever provider it belongs to. Returns how many were inserted.
pub fn add_subscription_models(
    reg: &mut ModelRegistry,
    provider: &str,
    base_url: &str,
    picks: &[ChatGptModelPick],
) -> Result<u32, String> {
    let mut seen = std::collections::HashSet::new();
    for pick in picks {
        validate_alias(&pick.alias)?;
        if pick.model.trim().is_empty() || pick.model.chars().any(char::is_control) {
            return Err(format!("model id {:?} is not usable", pick.model));
        }
        if !seen.insert(pick.alias.as_str()) {
            return Err(format!("alias {:?} given twice", pick.alias));
        }
    }
    let mut added = 0;
    for pick in picks {
        if !reg.models.contains_key(&pick.alias) {
            reg.models
                .insert(pick.alias.clone(), subscription_entry(provider, base_url, pick));
            added += 1;
        }
    }
    Ok(added)
}

pub fn add_chatgpt_models(reg: &mut ModelRegistry, picks: &[ChatGptModelPick]) -> Result<u32, String> {
    add_subscription_models(reg, "codex", CHATGPT_GATEWAY_BASE, picks)
}

/// Remove only what a subscription provider wrote: `provider == provider`
/// *and* subscription billing. A hand-authored entry without the billing
/// marker is left alone. Returns how many were removed.
pub fn disconnect_subscription(reg: &mut ModelRegistry, provider: &str) -> u32 {
    let before = reg.models.len();
    reg.models
        .retain(|_, e| !(e.provider == provider && e.billing == Some(BillingMode::Subscription)));
    (before - reg.models.len()) as u32
}

pub fn disconnect_chatgpt(reg: &mut ModelRegistry) -> u32 {
    disconnect_subscription(reg, "codex")
}
```

  The Tauri commands `chatgpt_models_add` / `chatgpt_disconnect` are
  unchanged. Add one test to the module:

```rust
    #[test]
    fn disconnect_is_scoped_to_its_own_provider() {
        let mut reg = ModelRegistry::default();
        add_chatgpt_models(&mut reg, &[pick("gpt-5.6-sol", "gpt", true)]).unwrap();
        add_subscription_models(
            &mut reg,
            "claude",
            "http://127.0.0.1:8088/v1",
            &[pick("claude-opus-5", "opus", true)],
        )
        .unwrap();
        assert_eq!(reg.models["opus"].provider, "claude");
        assert_eq!(reg.models["opus"].base_url.as_deref(), Some("http://127.0.0.1:8088/v1"));
        assert_eq!(disconnect_subscription(&mut reg, "claude"), 1);
        assert!(reg.models.contains_key("gpt"), "a ChatGPT entry survived a Claude disconnect");
        assert_eq!(disconnect_chatgpt(&mut reg), 1);
    }
```

- [x] Run `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml chatgpt_subscription`
  (expect the existing 12 plus the new test to pass) and
  `cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --all-targets -- -D warnings`;
  expect zero failures/warnings.

- [x] Commit: `git add mur-hub-gui/src-tauri/src/chatgpt_subscription`, then
  `git commit -m "refactor(hub): parameterize subscription plumbing by provider"`.

---

## Task 5 — Add the `claude_subscription` control plane and commands

**Interfaces**

Consumes: Task 4 aliases and helpers; `crate::cli_tools::shell_which`;
`mur_core::model_prices::{load_or_fetch, Catalog::provider_models}`.

Produces:

```rust
// mur-hub-gui/src-tauri/src/claude_subscription/mod.rs
pub const CLAUDE_GATEWAY_BASE: &str = "http://127.0.0.1:8088/v1";
pub fn resolve_claude() -> Option<PathBuf>;
#[tauri::command] pub async fn claude_account_read() -> Result<SubscriptionAccountView, String>;
#[tauri::command] pub async fn claude_login() -> Result<LoginResult, String>;
#[tauri::command] pub async fn claude_logout(confirmed: bool) -> Result<(), String>;
#[tauri::command] pub async fn claude_models_list() -> Result<Vec<SubscriptionModelView>, String>;
#[tauri::command] pub fn claude_models_add(picks: Vec<SubscriptionModelPick>) -> Result<(), String>;
#[tauri::command] pub fn claude_disconnect() -> Result<u32, String>;

// claude_subscription/account.rs
pub fn parse_auth_status(raw: &str) -> SubscriptionAccountView;
pub async fn read_account(claude: &Path) -> Result<SubscriptionAccountView, String>;
pub async fn login(claude: &Path) -> LoginResult;
pub async fn logout(claude: &Path, confirmed: bool) -> Result<(), String>;

// claude_subscription/catalog.rs
pub fn catalog_models(mur_home: &Path) -> Option<Vec<SubscriptionModelView>>;
pub fn models_from_ids(ids: Vec<String>) -> Vec<SubscriptionModelView>;
```

Gateway status/install are the existing `chatgpt_gateway_status` /
`chatgpt_gateway_install` commands — the UI descriptor names them.

**Steps**

- [x] Create `mur-hub-gui/src-tauri/src/claude_subscription/account.rs`:

```rust
//! `claude auth status / login / logout`, wrapped the way `codex` is.
//!
//! `claude auth status --json` is the account control plane. Only three
//! fields survive parsing — `loggedIn`, `authMethod`, `email` — the rest
//! (`orgId`, paths, flags) is dropped before it can reach a view or a log.
//! A subscription is `loggedIn && authMethod == "claude.ai"`; a Console
//! login (`console`) is API billing and renders as "signed in, but not this
//! provider", exactly like Codex `apiKey`.

use crate::chatgpt_subscription::process::{LOGOUT_CONFIRMATION_REQUIRED, LoginResult, run_bounded};
use crate::chatgpt_subscription::SubscriptionAccountView;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const SUBSCRIPTION_AUTH_METHOD: &str = "claude.ai";

/// Two Hub windows must not open two browser login flows.
static LOGIN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The JSON object inside possibly-noisy combined output (stderr banners,
/// update notices). Anything unparseable is "signed out", not an error:
/// the user can act on that, and the raw text never leaves this function.
pub fn parse_auth_status(raw: &str) -> SubscriptionAccountView {
    let present = SubscriptionAccountView {
        cli_present: true,
        ..Default::default()
    };
    let Some(json) = raw.find('{').zip(raw.rfind('}')).map(|(a, b)| &raw[a..=b]) else {
        return present;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return present;
    };
    let logged_in_any = v["loggedIn"].as_bool().unwrap_or(false);
    let method = v["authMethod"].as_str().map(str::to_string);
    let subscription = logged_in_any && method.as_deref() == Some(SUBSCRIPTION_AUTH_METHOD);
    SubscriptionAccountView {
        cli_present: true,
        logged_in: subscription,
        auth_mode: if logged_in_any { method } else { None },
        email: if subscription {
            v["email"].as_str().map(str::to_string)
        } else {
            None
        },
        plan_type: None,
    }
}

pub async fn read_account(claude: &Path) -> Result<SubscriptionAccountView, String> {
    let mut cmd = Command::new(claude);
    cmd.args(["auth", "status", "--json"]);
    // Exit code deliberately ignored: a signed-out status may exit non-zero
    // and still carry the JSON that says so.
    let (_ok, out) = run_bounded(cmd, STATUS_TIMEOUT).await?;
    Ok(parse_auth_status(&out))
}

/// `claude auth login --claudeai`, then ask the account — exit code zero
/// alone is not success, and a Console login is not this provider.
pub async fn login(claude: &Path) -> LoginResult {
    let _one_at_a_time = LOGIN_LOCK.lock().await;
    let failed = |error: String| LoginResult {
        authenticated: false,
        error: Some(error),
    };
    let mut cmd = Command::new(claude);
    cmd.args(["auth", "login", "--claudeai"]);
    let output = match run_bounded(cmd, LOGIN_TIMEOUT).await {
        Ok((true, out)) => out,
        Ok((false, out)) => return failed(format!("claude auth login failed: {}", out.trim())),
        Err(e) => return failed(format!("claude auth login: {e}")),
    };
    match read_account(claude).await {
        Ok(a) if a.logged_in => LoginResult {
            authenticated: true,
            error: None,
        },
        Ok(a) => failed(format!(
            "claude auth login finished but no Claude subscription is signed in (auth: {}). {}",
            a.auth_mode.as_deref().unwrap_or("none"),
            output.trim()
        )),
        Err(e) => failed(e),
    }
}

/// Global sign-out. Refuses without `confirmed` — nothing is spawned.
pub async fn logout(claude: &Path, confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err(LOGOUT_CONFIRMATION_REQUIRED.into());
    }
    let mut cmd = Command::new(claude);
    cmd.args(["auth", "logout"]);
    let (ok, out) = run_bounded(cmd, STATUS_TIMEOUT).await?;
    if !ok {
        return Err(format!("claude auth logout failed: {}", out.trim()));
    }
    match read_account(claude).await {
        Ok(a) if a.logged_in => Err("claude auth logout ran but a subscription is still signed in".into()),
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_claude_ai_login_is_a_subscription_and_nothing_else_survives() {
        let sub = parse_auth_status(
            r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"u@example.com","orgId":"org-SECRET","projectsDirectory":"/Users/x/.claude/projects"}"#,
        );
        assert_eq!(
            sub,
            SubscriptionAccountView {
                cli_present: true,
                logged_in: true,
                auth_mode: Some("claude.ai".into()),
                email: Some("u@example.com".into()),
                plan_type: None,
            }
        );
        assert!(!format!("{sub:?}").contains("SECRET"), "orgId leaked into the view");

        let console = parse_auth_status(r#"{"loggedIn":true,"authMethod":"console","email":"api@example.com"}"#);
        assert!(!console.logged_in, "a Console login is API billing, not a subscription");
        assert_eq!(console.auth_mode.as_deref(), Some("console"));
        assert_eq!(console.email, None, "a non-subscription identity is not this provider's");

        let out = parse_auth_status(r#"{"loggedIn":false}"#);
        assert!(!out.logged_in);
        assert_eq!(out.auth_mode, None);

        let noisy = parse_auth_status("Update available: 2.2.0\n{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}\n");
        assert!(noisy.logged_in);

        let garbage = parse_auth_status("not json at all");
        assert!(garbage.cli_present && !garbage.logged_in);
        let unknown_method = parse_auth_status(r#"{"loggedIn":true,"authMethod":"something-new"}"#);
        assert!(!unknown_method.logged_in, "an unknown method is never assumed to be a subscription");
    }
}
```

- [x] Create `mur-hub-gui/src-tauri/src/claude_subscription/catalog.rs`:

```rust
//! Models on a Claude plan come from the models.dev catalog — the same
//! source the Hub's Anthropic discovery already uses instead of probing the
//! endpoint. There is no `/v1/models` call: the gateway would forward it
//! with the OAuth token, and the endpoint is not part of the subscription
//! contract.

use crate::chatgpt_subscription::SubscriptionModelView;
use std::path::Path;

const CATALOG_VENDOR: &str = "anthropic";
const DEFAULT_INPUT_MODALITIES: [&str; 2] = ["text", "image"];

/// `None` when no catalog is reachable (fresh cache, network, stale cache
/// all failed) — the panel then offers the unverified-id field.
pub fn catalog_models(mur_home: &Path) -> Option<Vec<SubscriptionModelView>> {
    let ids = mur_core::model_prices::load_or_fetch(mur_home)?.provider_models(CATALOG_VENDOR)?;
    Some(models_from_ids(ids))
}

/// The catalog has no display name, default marker, or effort list, so
/// every row is the id and nothing is pre-selected.
pub fn models_from_ids(ids: Vec<String>) -> Vec<SubscriptionModelView> {
    ids.into_iter()
        .map(|id| SubscriptionModelView {
            display_name: id.clone(),
            id,
            is_default: false,
            reasoning_efforts: vec![],
            input_modalities: DEFAULT_INPUT_MODALITIES.map(String::from).to_vec(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_become_plain_rows_with_no_default() {
        let rows = models_from_ids(vec!["claude-opus-5".into(), "claude-sonnet-5".into()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "claude-opus-5");
        assert_eq!(rows[0].display_name, "claude-opus-5");
        assert!(rows.iter().all(|r| !r.is_default));
        assert_eq!(rows[1].input_modalities, vec!["text", "image"]);
    }

    /// A seeded cache is read without touching the network; the vendor key
    /// is `anthropic`, and other vendors' models do not leak in.
    #[test]
    fn catalog_models_come_from_the_cached_anthropic_entry() {
        let home = tempfile::tempdir().unwrap();
        let path = mur_core::model_prices::cache_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
              "anthropic": { "models": { "claude-b": {}, "claude-a": {} } },
              "openai": { "models": { "gpt-x": {} } }
            }"#,
        )
        .unwrap();
        let rows = catalog_models(home.path()).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["claude-a", "claude-b"], "sorted, anthropic only");
    }
}
```

- [x] Create `mur-hub-gui/src-tauri/src/claude_subscription/mod.rs`:

```rust
//! Claude Subscription provider — the Hub side. Sibling of
//! `chatgpt_subscription`; shares its views, gateway lifecycle, registry
//! rules, and bounded child runner. Claude Code owns the login; this module
//! never reads the keychain blob or the credentials file.

pub mod account;
pub mod catalog;

use crate::chatgpt_subscription::process::LoginResult;
use crate::chatgpt_subscription::registry::{add_subscription_models, disconnect_subscription};
use crate::chatgpt_subscription::{
    SubscriptionAccountView, SubscriptionModelPick, SubscriptionModelView,
};
use mur_common::model::ModelRegistry;
use std::path::PathBuf;

/// The gateway's Anthropic route; the runtime appends `/messages`.
pub const CLAUDE_GATEWAY_BASE: &str = "http://127.0.0.1:8088/v1";
const CLAUDE_PROVIDER: &str = "claude";
const CLAUDE_BIN: &str = "claude";
const CLI_MISSING: &str = "claude CLI not found on PATH";

/// The `claude` the user's shell would run — same discipline as `codex`.
pub fn resolve_claude() -> Option<PathBuf> {
    #[cfg(unix)]
    if let Some(p) = crate::cli_tools::shell_which(CLAUDE_BIN) {
        return Some(p);
    }
    let home = dirs::home_dir()?;
    [
        home.join(".local/bin").join(CLAUDE_BIN),
        PathBuf::from("/opt/homebrew/bin").join(CLAUDE_BIN),
        PathBuf::from("/usr/local/bin").join(CLAUDE_BIN),
        home.join(".npm-global/bin").join(CLAUDE_BIN),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

async fn claude_or_err() -> Result<PathBuf, String> {
    tokio::task::spawn_blocking(resolve_claude)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| CLI_MISSING.to_string())
}

fn mur_home() -> PathBuf {
    ModelRegistry::default_path()
        .ok()
        .and_then(|p| p.parent().map(|x| x.to_path_buf()))
        .unwrap_or_default()
}

#[tauri::command]
pub async fn claude_account_read() -> Result<SubscriptionAccountView, String> {
    let Ok(claude) = claude_or_err().await else {
        return Ok(SubscriptionAccountView::default());
    };
    account::read_account(&claude).await
}

#[tauri::command]
pub async fn claude_login() -> Result<LoginResult, String> {
    Ok(account::login(&claude_or_err().await?).await)
}

#[tauri::command]
pub async fn claude_logout(confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err(crate::chatgpt_subscription::process::LOGOUT_CONFIRMATION_REQUIRED.into());
    }
    account::logout(&claude_or_err().await?, true).await
}

#[tauri::command]
pub async fn claude_models_list() -> Result<Vec<SubscriptionModelView>, String> {
    let home = mur_home();
    // `load_or_fetch` may block on the network; keep it off the executor.
    tokio::task::spawn_blocking(move || catalog::catalog_models(&home))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the models.dev catalog is not reachable and no cached copy exists".to_string())
}

#[tauri::command]
pub fn claude_models_add(picks: Vec<SubscriptionModelPick>) -> Result<(), String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    add_subscription_models(&mut reg, CLAUDE_PROVIDER, CLAUDE_GATEWAY_BASE, &picks)?;
    reg.save_to(&path).map_err(|e| e.to_string())
}

/// Registry entries only. The Claude Code login and the gateway are
/// untouched — every other Claude Code client keeps working.
#[tauri::command]
pub fn claude_disconnect() -> Result<u32, String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    let removed = disconnect_subscription(&mut reg, CLAUDE_PROVIDER);
    if removed > 0 {
        reg.save_to(&path).map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A fake `claude` that records argv and answers `auth status` with the
    /// JSON `body`; `auth login` / `auth logout` exit with `login_exit`.
    fn fake_claude(dir: &tempfile::TempDir, status_json: &str, login_exit: u8) -> (PathBuf, PathBuf) {
        let marker = dir.path().join("invoked");
        let bin = dir.path().join("claude");
        let src = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1 $2\" in\n  'auth status') printf '%s\\n' '{}';;\n  'auth login'|'auth logout') exit {};;\nesac\n",
            marker.display(),
            status_json,
            login_exit
        );
        std::fs::File::create(&bin).unwrap().write_all(src.as_bytes()).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (bin, marker)
    }

    #[tokio::test]
    async fn status_login_and_logout_believe_the_account() {
        let _serial = crate::chatgpt_subscription::FAKE_BIN_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();

        // Console login: `login` exits 0 but the account is not a subscription.
        let (bin, marker) = fake_claude(&dir, r#"{"loggedIn":true,"authMethod":"console"}"#, 0);
        let view = account::read_account(&bin).await.unwrap();
        assert!(view.cli_present && !view.logged_in);
        let r = account::login(&bin).await;
        assert!(!r.authenticated);
        assert!(r.error.unwrap().contains("console"));
        let argv = std::fs::read_to_string(&marker).unwrap();
        assert!(argv.contains("auth status --json"), "{argv}");
        assert!(argv.contains("auth login --claudeai"), "{argv}");

        // Subscription login succeeds only because status says so.
        let dir = tempfile::tempdir().unwrap();
        let (bin, _) = fake_claude(&dir, r#"{"loggedIn":true,"authMethod":"claude.ai","email":"u@example.com"}"#, 0);
        assert!(account::login(&bin).await.authenticated);
        // Logout ran, but status still says signed in → error, not success.
        assert!(account::logout(&bin, true).await.is_err());

        // No confirmation → nothing spawned.
        let dir = tempfile::tempdir().unwrap();
        let (bin, marker) = fake_claude(&dir, r#"{"loggedIn":false}"#, 0);
        assert_eq!(
            account::logout(&bin, false).await.err().unwrap(),
            crate::chatgpt_subscription::process::LOGOUT_CONFIRMATION_REQUIRED
        );
        assert!(!marker.exists(), "a process was spawned without confirmation");
        // Confirmed logout with a signed-out status is a clean success.
        assert!(account::logout(&bin, true).await.is_ok());

        // Missing binary is a spawn error, not a panic.
        assert!(account::read_account(std::path::Path::new("/nonexistent/claude")).await.is_err());
    }
}
```

- [x] In `mur-hub-gui/src-tauri/src/lib.rs`: add `pub mod claude_subscription;`
  after `pub mod chatgpt_subscription;`, and register the six commands
  directly after `chatgpt_subscription::registry::chatgpt_disconnect,`:

```rust
            claude_subscription::claude_account_read,
            claude_subscription::claude_login,
            claude_subscription::claude_logout,
            claude_subscription::claude_models_list,
            claude_subscription::claude_models_add,
            claude_subscription::claude_disconnect,
```

- [x] Run
  `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml claude_subscription`
  (expect 4 tests: `parse_auth_status`, the two catalog tests, and the
  fake-`claude` flow), then the full
  `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml` and
  `cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --all-targets -- -D warnings`;
  expect zero failures/warnings. The `#[cfg(all(test, unix))]` gate on the
  spawning test mirrors the ChatGPT modules (Windows CI compiles the crate
  without them; a `cfg(test)`-only item that is unused there fails
  `-D warnings`).

- [x] Commit: `git add mur-hub-gui/src-tauri/src/claude_subscription mur-hub-gui/src-tauri/src/lib.rs`,
  then `git commit -m "feat(hub): Claude subscription account, catalog, and registry commands"`.

---

## Task 6 — Descriptor-driven subscription panel

**Interfaces**

Consumes: Task 5 command names; existing `ChatGPTSubscriptionPanel` and
`chatgptSubscription.ts` from mur#1154.

Produces (TypeScript):

```ts
// chatgptSubscription.ts
export interface GatewayStatus { …existing; claude_credential_mode?: string | null }
export interface GatewayReadiness { requiresHook: boolean; credential: "codex" | "claude" }
export const CHATGPT_READINESS: GatewayReadiness; // { requiresHook: true,  credential: "codex" }
export const CLAUDE_READINESS: GatewayReadiness;  // { requiresHook: false, credential: "claude" }
export function gatewayProblem(g: GatewayStatus, r: GatewayReadiness): GatewayProblem | null;
export function deriveSubscriptionState(input: ChatGPTStateInput, r: GatewayReadiness): ChatGPTPanelState;
export function deriveChatGPTState(input: ChatGPTStateInput): ChatGPTPanelState; // = deriveSubscriptionState(input, CHATGPT_READINESS)

// modelLibraryHelpers.ts
export interface SubscriptionDescriptor {
  key: string; provider: string; name: string; logo: string; color: string;
  readiness: GatewayReadiness;
  commands: { accountRead: string; modelsList: string; login: string; logout: string; modelsAdd: string; disconnect: string };
  copy: SubscriptionCopy;            // Record<SubscriptionCopyKey, TranslationKey>
  cliInstallCmd: string;             // shown verbatim beside copy.cliInstallHint
}
export const CHATGPT_SUBSCRIPTION: SubscriptionDescriptor;
export const CLAUDE_SUBSCRIPTION: SubscriptionDescriptor;
export const SUBSCRIPTION_PROVIDERS: readonly SubscriptionDescriptor[];

// SubscriptionProviderPanel.tsx
export function SubscriptionProviderPanel(props: { descriptor: SubscriptionDescriptor; registryModels: ModelOption[]; onModelsAdded(): void }): JSX.Element;
```

`SubscriptionCopyKey` is exactly: `name | subtitle | billingNote | cliMissing |
cliInstallHint | loggedOut | loggedOutApiBilled | loginBtn | loginInProgress |
loginFailed | accountUnavailable | modelsTitle | modelsHint | registryTitle |
disconnectBtn | disconnectHint | logoutBtn | logoutConfirmTitle |
logoutConfirmBody | logoutConfirmOk`. Gateway, retry, advanced-id, badge,
and add-button copy stays shared under `lib.chatgpt.gateway.*` /
`lib.chatgpt.*` as today.

**Steps**

- [x] In `chatgptSubscription.test.ts`, add after the existing
  `subscription readiness is strict` block:

```ts
describe("readiness is descriptor-driven", () => {
  const claudeReady: GatewayStatus = {
    installed: true,
    running: true,
    codex_hook: false,
    credential_mode: "missing",
    claude_credential_mode: "oauth",
    compression: false,
  };
  it("Claude ignores the codex hook and codex credential, requires claudeCredential oauth", () => {
    expect(gatewayProblem(claudeReady, CLAUDE_READINESS)).toBeNull();
    expect(gatewayProblem({ ...claudeReady, claude_credential_mode: "missing" }, CLAUDE_READINESS)).toBe("credential-missing");
    expect(gatewayProblem({ ...claudeReady, claude_credential_mode: null }, CLAUDE_READINESS)).toBe("credential-missing");
    expect(gatewayProblem({ ...claudeReady, running: false }, CLAUDE_READINESS)).toBe("not-running");
  });
  it("ChatGPT still requires the hook and a chatgpt credential", () => {
    expect(gatewayProblem(claudeReady, CHATGPT_READINESS)).toBe("hook-missing");
    expect(gatewayProblem({ ...claudeReady, codex_hook: true }, CHATGPT_READINESS)).toBe("credential-missing");
    expect(gatewayProblem({ ...claudeReady, codex_hook: true, credential_mode: "chatgpt" }, CHATGPT_READINESS)).toBeNull();
  });
  it("deriveChatGPTState is deriveSubscriptionState with the ChatGPT rule", () => {
    const input = { ...base, gateway: claudeReady };
    expect(deriveChatGPTState(input)).toEqual(deriveSubscriptionState(input, CHATGPT_READINESS));
    expect(deriveSubscriptionState(input, CLAUDE_READINESS).kind).toBe("ready");
    expect(deriveChatGPTState(input).kind).toBe("gateway-stopped");
  });
});
```

  Extend the import at the top of the file with `CHATGPT_READINESS,
  CLAUDE_READINESS, deriveSubscriptionState, gatewayProblem`.

- [x] Run `npm test -- chatgptSubscription.test.ts` in `mur-hub-gui/ui` and
  watch it fail (exports absent).

- [x] In `chatgptSubscription.ts`: add `claude_credential_mode?: string | null;`
  to `GatewayStatus` (after `credential_mode`), and replace `gatewayProblem`
  and `deriveChatGPTState` with:

```ts
/** What a gateway must report before this provider may route through it. */
export interface GatewayReadiness {
  /** ChatGPT needs the compiled Codex hook; the Anthropic path is plain header attachment. */
  requiresHook: boolean;
  credential: "codex" | "claude";
}

export const CHATGPT_READINESS: GatewayReadiness = { requiresHook: true, credential: "codex" };
export const CLAUDE_READINESS: GatewayReadiness = { requiresHook: false, credential: "claude" };

export function gatewayProblem(g: GatewayStatus, r: GatewayReadiness): GatewayProblem | null {
  if (!g.running) return "not-running";
  if (r.requiresHook && !g.codex_hook) return "hook-missing";
  if (r.credential === "codex") {
    if (g.credential_mode === "apikey") return "credential-apikey";
    if (g.credential_mode !== "chatgpt") return "credential-missing";
    return null;
  }
  if (g.claude_credential_mode !== "oauth") return "credential-missing";
  return null;
}

/**
 * Precedence, first match wins:
 * login in progress → missing CLI → logged out → account error → loading →
 * gateway missing → gateway stopped/unusable → models loading → ready.
 */
export function deriveSubscriptionState(
  input: ChatGPTStateInput,
  readiness: GatewayReadiness,
): ChatGPTPanelState {
  const { account, accountError, loginInProgress, gateway, models, modelsError } = input;
  if (loginInProgress) return { kind: "login-in-progress" };
  if (account && !account.cli_present) return { kind: "codex-missing" };
  if (account && !account.logged_in) return { kind: "logged-out" };
  if (accountError) return { kind: "account-unavailable", message: accountError };
  if (!account) return { kind: "loading" };
  if (!gateway) return { kind: "loading" };
  if (!gateway.installed) return { kind: "gateway-missing", account };
  const problem = gatewayProblem(gateway, readiness);
  if (problem) return { kind: "gateway-stopped", account, problem };
  if (models === null && !modelsError) return { kind: "models-loading", account };
  return { kind: "ready", account, models: models ?? [], modelsError };
}

export function deriveChatGPTState(input: ChatGPTStateInput): ChatGPTPanelState {
  return deriveSubscriptionState(input, CHATGPT_READINESS);
}
```

  Update the doc comment on `codex-missing` in `ChatGPTPanelState` to
  `/** The provider's CLI (codex / claude) is not on PATH. */` — the kind
  string itself stays `codex-missing` so the ChatGPT tests remain byte-for-byte.

- [x] Run `npm test -- chatgptSubscription.test.ts`; expect all tests (the
  17 existing + 3 new) to pass.

- [x] In `modelLibraryHelpers.ts`, replace the `CHATGPT_SUBSCRIPTION` block
  with:

```ts
import type { TranslationKey } from "../i18n/types";
import type { GatewayReadiness } from "./chatgptSubscription";
import { CHATGPT_READINESS, CLAUDE_READINESS } from "./chatgptSubscription";

export type SubscriptionCopyKey =
  | "name"
  | "subtitle"
  | "billingNote"
  | "cliMissing"
  | "cliInstallHint"
  | "loggedOut"
  | "loggedOutApiBilled"
  | "loginBtn"
  | "loginInProgress"
  | "loginFailed"
  | "accountUnavailable"
  | "modelsTitle"
  | "modelsHint"
  | "registryTitle"
  | "disconnectBtn"
  | "disconnectHint"
  | "logoutBtn"
  | "logoutConfirmTitle"
  | "logoutConfirmBody"
  | "logoutConfirmOk";

export type SubscriptionCopy = Record<SubscriptionCopyKey, TranslationKey>;

/**
 * A subscription provider is deliberately not a CLOUD_PRESET: it has no API
 * key, no base URL to edit, and must never be tested against the vendor
 * host. Each one gets its own rail entry and shares SubscriptionProviderPanel.
 */
export interface SubscriptionDescriptor {
  key: string;
  /** The wire provider its registry entries carry. */
  provider: string;
  name: string;
  logo: string;
  color: string;
  readiness: GatewayReadiness;
  commands: {
    accountRead: string;
    modelsList: string;
    login: string;
    logout: string;
    modelsAdd: string;
    disconnect: string;
  };
  copy: SubscriptionCopy;
  /** Shown verbatim beside `copy.cliInstallHint`. */
  cliInstallCmd: string;
}

export const CHATGPT_SUBSCRIPTION: SubscriptionDescriptor = {
  key: "chatgpt-subscription",
  provider: "codex",
  name: "ChatGPT Subscription",
  logo: "GPT",
  color: "#10A37F",
  readiness: CHATGPT_READINESS,
  commands: {
    accountRead: "chatgpt_account_read",
    modelsList: "chatgpt_models_list",
    login: "chatgpt_login",
    logout: "chatgpt_logout",
    modelsAdd: "chatgpt_models_add",
    disconnect: "chatgpt_disconnect",
  },
  copy: {
    name: "lib.chatgpt.name",
    subtitle: "lib.chatgpt.subtitle",
    billingNote: "lib.chatgpt.billingNote",
    cliMissing: "lib.chatgpt.codexMissing",
    cliInstallHint: "lib.chatgpt.codexInstallHint",
    loggedOut: "lib.chatgpt.loggedOut",
    loggedOutApiBilled: "lib.chatgpt.loggedOutApiBilled",
    loginBtn: "lib.chatgpt.loginBtn",
    loginInProgress: "lib.chatgpt.loginInProgress",
    loginFailed: "lib.chatgpt.loginFailed",
    accountUnavailable: "lib.chatgpt.accountUnavailable",
    modelsTitle: "lib.chatgpt.modelsTitle",
    modelsHint: "lib.chatgpt.modelsHint",
    registryTitle: "lib.chatgpt.registryTitle",
    disconnectBtn: "lib.chatgpt.disconnectBtn",
    disconnectHint: "lib.chatgpt.disconnectHint",
    logoutBtn: "lib.chatgpt.logoutBtn",
    logoutConfirmTitle: "lib.chatgpt.logoutConfirmTitle",
    logoutConfirmBody: "lib.chatgpt.logoutConfirmBody",
    logoutConfirmOk: "lib.chatgpt.logoutConfirmOk",
  },
  cliInstallCmd: "npm install -g @openai/codex",
};

export const CLAUDE_SUBSCRIPTION: SubscriptionDescriptor = {
  key: "claude-subscription",
  provider: "claude",
  name: "Claude Subscription",
  logo: "CL",
  color: "#C5694A",
  readiness: CLAUDE_READINESS,
  commands: {
    accountRead: "claude_account_read",
    modelsList: "claude_models_list",
    login: "claude_login",
    logout: "claude_logout",
    modelsAdd: "claude_models_add",
    disconnect: "claude_disconnect",
  },
  copy: {
    name: "lib.claude.name",
    subtitle: "lib.claude.subtitle",
    billingNote: "lib.claude.billingNote",
    cliMissing: "lib.claude.cliMissing",
    cliInstallHint: "lib.claude.cliInstallHint",
    loggedOut: "lib.claude.loggedOut",
    loggedOutApiBilled: "lib.claude.loggedOutApiBilled",
    loginBtn: "lib.claude.loginBtn",
    loginInProgress: "lib.claude.loginInProgress",
    loginFailed: "lib.claude.loginFailed",
    accountUnavailable: "lib.claude.accountUnavailable",
    modelsTitle: "lib.claude.modelsTitle",
    modelsHint: "lib.claude.modelsHint",
    registryTitle: "lib.claude.registryTitle",
    disconnectBtn: "lib.claude.disconnectBtn",
    disconnectHint: "lib.claude.disconnectHint",
    logoutBtn: "lib.claude.logoutBtn",
    logoutConfirmTitle: "lib.claude.logoutConfirmTitle",
    logoutConfirmBody: "lib.claude.logoutConfirmBody",
    logoutConfirmOk: "lib.claude.logoutConfirmOk",
  },
  cliInstallCmd: "npm install -g @anthropic-ai/claude-code",
};

export const SUBSCRIPTION_PROVIDERS: readonly SubscriptionDescriptor[] = [
  CHATGPT_SUBSCRIPTION,
  CLAUDE_SUBSCRIPTION,
];
```

  Place the three `import` lines at the top of the file (it currently has
  none). Add a test to `modelLibraryHelpers.test.ts`:

```ts
import { SUBSCRIPTION_PROVIDERS } from "./modelLibraryHelpers";
import { en } from "../i18n/en";
import { zhTW } from "../i18n/zh-TW";

describe("subscription descriptors", () => {
  it("every copy key resolves in both languages and providers are distinct", () => {
    const keys = new Set<string>();
    for (const d of SUBSCRIPTION_PROVIDERS) {
      expect(keys.has(d.key)).toBe(false);
      keys.add(d.key);
      for (const k of Object.values(d.copy)) {
        expect(en[k], `${d.key}: ${k} missing in en`).toBeTruthy();
        expect(zhTW[k], `${d.key}: ${k} missing in zh-TW`).toBeTruthy();
      }
    }
    expect(new Set(SUBSCRIPTION_PROVIDERS.map((d) => d.provider)).size).toBe(SUBSCRIPTION_PROVIDERS.length);
  });
});
```

  (`zhTW` is the table's export name in `src/i18n/zh-TW.ts`; `en` in `en.ts`.)

- [x] `git mv src/components/ChatGPTSubscriptionPanel.tsx src/components/SubscriptionProviderPanel.tsx`
  and make these edits in the moved file (every other line stays):
  - Rename the component: `export function SubscriptionProviderPanel({ descriptor, registryModels, onModelsAdded }: { descriptor: SubscriptionDescriptor; registryModels: ModelOption[]; onModelsAdded: () => void })`.
  - Imports: replace `import { CHATGPT_SUBSCRIPTION, togglePick } from "./modelLibraryHelpers";`
    with `import { togglePick, type SubscriptionDescriptor } from "./modelLibraryHelpers";`
    and replace `deriveChatGPTState,` / `gatewayProblem,` in the
    `./chatgptSubscription` import with `deriveSubscriptionState,` /
    `gatewayProblem,` (keep the rest).
  - Every `invoke<…>("chatgpt_account_read")` → `invoke<…>(descriptor.commands.accountRead)`;
    `"chatgpt_gateway_status"` and `"chatgpt_gateway_install"` stay literal;
    `"chatgpt_models_list"` → `descriptor.commands.modelsList`;
    `"chatgpt_login"` → `descriptor.commands.login`;
    `"chatgpt_logout"` → `descriptor.commands.logout`;
    `"chatgpt_models_add"` → `descriptor.commands.modelsAdd`;
    `"chatgpt_disconnect"` → `descriptor.commands.disconnect`.
  - `const gatewayUsable = gateway !== null && gatewayProblem(gateway) === null;`
    → `… gatewayProblem(gateway, descriptor.readiness) === null;`
  - `const state = deriveChatGPTState({…})` → `deriveSubscriptionState({…}, descriptor.readiness)`.
  - `registryModels.filter((m) => m.provider === "codex")` → `m.provider === descriptor.provider`.
  - Every `t("lib.chatgpt.<k>")` for a key in `SubscriptionCopyKey` → `t(descriptor.copy.<k>)`:
    `name`, `subtitle`, `billingNote`, `codexMissing`→`cliMissing`,
    `codexInstallHint`→`cliInstallHint`, `loggedOut`, `loginBtn`,
    `loginInProgress`, `loginFailed` (both call sites, keep the `{ error }`
    argument), `accountUnavailable`, `modelsTitle` (three sites),
    `modelsHint`, `registryTitle`, `disconnectBtn`, `disconnectHint`,
    `logoutBtn`, `logoutConfirmTitle`, `logoutConfirmBody`, `logoutConfirmOk`.
    The literal `<code className="ml-code">npm install -g @openai/codex</code>`
    becomes `<code className="ml-code">{descriptor.cliInstallCmd}</code>`.
    `CHATGPT_SUBSCRIPTION.color` / `.logo` in the header → `descriptor.color` / `descriptor.logo`.
  - `Body` gains a prop `copy: SubscriptionCopy` and `cliInstallCmd: string`
    (pass `copy={descriptor.copy} cliInstallCmd={descriptor.cliInstallCmd}`),
    and its `logged-out` arm distinguishes a non-subscription login:

```tsx
    case "logged-out":
      return (
        <div>
          <p className="ml-hint">
            {t(apiBilled ? copy.loggedOutApiBilled : copy.loggedOut)}
          </p>
          <button className="ml-btn ml-btn--primary" disabled={busy} onClick={onLogin}>
            {t(copy.loginBtn)}
          </button>
        </div>
      );
```

    where `Body` also receives `apiBilled: boolean` computed in the parent as
    `const apiBilled = account?.auth_mode != null && !account.logged_in;`.
    Add `import type { SubscriptionCopy } from "./modelLibraryHelpers";`.
  - `ModelSection` receives `copy` as well for `modelsTitle` / `modelsHint`.

- [x] In `ModelLibrary.tsx`:
  - imports: `import { CLOUD_PRESETS, SUBSCRIPTION_PROVIDERS, type SubscriptionDescriptor } from "./modelLibraryHelpers";`
    and `import { SubscriptionProviderPanel } from "./SubscriptionProviderPanel";`
    (remove the `ChatGPTSubscriptionPanel` and `CHATGPT_SUBSCRIPTION` imports).
  - `PanelKind`: replace `| { kind: "chatgpt-subscription" }` with
    `| { kind: "subscription"; key: string }`.
  - Add `const subscriptionFor = (provider: string): SubscriptionDescriptor | undefined =>
    SUBSCRIPTION_PROVIDERS.find((d) => d.provider === provider);`
  - `deriveConnected`: the `key === CHATGPT_SUBSCRIPTION.provider` branch
    becomes a lookup: `const d = subscriptionFor(key); return d ? { key, name: d.name, color: d.color, initials: d.logo, modelCount: count } : { …generic }`.
  - `panelForConnected(providerKey)`: `const d = subscriptionFor(providerKey);
    return d ? { kind: "subscription", key: d.key } : { kind: "connected", providerKey };`
    and the rail's `active` test for a connected row becomes
    `target.kind === "subscription" ? panel?.kind === "subscription" && panel.key === target.key : …`.
  - The **Add Provider** section renders one button per descriptor whose
    provider is not already connected:

```tsx
            {SUBSCRIPTION_PROVIDERS.filter((d) => !connected.some((cp) => cp.key === d.provider)).map((d) => (
              <button
                key={d.key}
                className={`ml-prov ml-prov--add${panel?.kind === "subscription" && panel.key === d.key ? " ml-prov--active" : ""}`}
                onClick={() => setPanel({ kind: "subscription", key: d.key })}
                title={d.name}
              >
                <span className="ml-logo" style={{ background: d.color }} aria-hidden="true">
                  {d.logo}
                </span>
                <span className="ml-prov__name ml-prov__name--link">{d.name}</span>
              </button>
            ))}
```

  - Routing: `panel.kind === "subscription" ? (<SubscriptionProviderPanel descriptor={SUBSCRIPTION_PROVIDERS.find((d) => d.key === panel.key)!} registryModels={registryModels} onModelsAdded={…same as today…} />)`.

- [x] Add copy. In `en.ts`, directly after `"lib.chatgpt.logoutConfirmOk": "Sign out",`:

```ts
  "lib.chatgpt.loggedOutApiBilled": "Codex is signed in with an OpenAI API key. That is usage billing, not a ChatGPT subscription — sign in with ChatGPT to use this provider.",
  // ── Claude Subscription provider ──
  "lib.claude.name": "Claude Subscription",
  "lib.claude.subtitle": "Use your Claude Pro/Max plan through Claude Code — no API key, no usage billing.",
  "lib.claude.billingNote": "Requests are covered by your Claude subscription. This is not the Anthropic API: nothing here is usage-billed, and MUR will never add a usage-billed model as a fallback on its own.",
  "lib.claude.cliMissing": "Claude Code is not installed. MUR uses Claude Code to sign in to your Claude account.",
  "lib.claude.cliInstallHint": "Install it, then come back:",
  "lib.claude.loggedOut": "Not signed in to Claude. Sign-in opens in your browser and is handled by Claude Code; MUR never sees your password or token.",
  "lib.claude.loggedOutApiBilled": "Claude Code is signed in to the Anthropic Console. That is API billing, not a Claude subscription — sign in with your Claude account to use this provider.",
  "lib.claude.loginBtn": "Sign in with Claude",
  "lib.claude.loginInProgress": "Finish signing in in your browser…",
  "lib.claude.loginFailed": "Sign-in failed: {error}",
  "lib.claude.accountUnavailable": "Could not read the Claude Code account: {error}",
  "lib.claude.modelsTitle": "Claude models",
  "lib.claude.modelsHint": "From the models.dev catalog. Select the ones to add to the registry.",
  "lib.claude.registryTitle": "In your registry (Claude)",
  "lib.claude.disconnectBtn": "Disconnect MUR",
  "lib.claude.disconnectHint": "Removes the Claude subscription models from MUR's registry only. Your Claude Code login and other Claude Code sessions are untouched.",
  "lib.claude.logoutBtn": "Sign out of Claude",
  "lib.claude.logoutConfirmTitle": "Sign out of Claude?",
  "lib.claude.logoutConfirmBody": "This signs Claude Code out everywhere on this machine — terminal sessions and IDE extensions included — not just MUR.",
  "lib.claude.logoutConfirmOk": "Sign out",
```

  In `zh-TW.ts`, directly after `"lib.chatgpt.logoutConfirmOk": "登出",`:

```ts
  "lib.chatgpt.loggedOutApiBilled": "Codex 目前是以 OpenAI API key 登入。那是按用量計費，不是 ChatGPT 訂閱 — 請改用 ChatGPT 登入才能使用這個 provider。",
  // ── Claude Subscription provider ──
  "lib.claude.name": "Claude 訂閱",
  "lib.claude.subtitle": "透過 Claude Code 使用你的 Claude Pro/Max 方案 — 不需 API key，不按用量計費。",
  "lib.claude.billingNote": "請求由你的 Claude 訂閱涵蓋。這不是 Anthropic API：這裡沒有任何按用量計費的項目，MUR 也絕不會自行把按用量計費的模型加進備援鏈。",
  "lib.claude.cliMissing": "尚未安裝 Claude Code。MUR 透過 Claude Code 登入你的 Claude 帳號。",
  "lib.claude.cliInstallHint": "安裝後再回來：",
  "lib.claude.loggedOut": "尚未登入 Claude。登入會在瀏覽器中進行並由 Claude Code 處理；MUR 永遠不會看到你的密碼或 token。",
  "lib.claude.loggedOutApiBilled": "Claude Code 目前登入的是 Anthropic Console。那是 API 計費，不是 Claude 訂閱 — 請改用你的 Claude 帳號登入才能使用這個 provider。",
  "lib.claude.loginBtn": "使用 Claude 登入",
  "lib.claude.loginInProgress": "請在瀏覽器中完成登入…",
  "lib.claude.loginFailed": "登入失敗：{error}",
  "lib.claude.accountUnavailable": "無法讀取 Claude Code 帳號：{error}",
  "lib.claude.modelsTitle": "Claude 模型",
  "lib.claude.modelsHint": "來自 models.dev 目錄。勾選要加入 registry 的模型。",
  "lib.claude.registryTitle": "已在你的 registry（Claude）",
  "lib.claude.disconnectBtn": "中斷 MUR 連線",
  "lib.claude.disconnectHint": "只會從 MUR 的 registry 移除 Claude 訂閱模型。你的 Claude Code 登入與其他 Claude Code 工作階段不受影響。",
  "lib.claude.logoutBtn": "登出 Claude",
  "lib.claude.logoutConfirmTitle": "要登出 Claude 嗎？",
  "lib.claude.logoutConfirmBody": "這會讓這台機器上的 Claude Code 全部登出 — 包括終端機工作階段與 IDE 擴充功能 — 不只是 MUR。",
  "lib.claude.logoutConfirmOk": "登出",
```

- [x] Run in `mur-hub-gui/ui`: `npm test`, `npm run lint`, `npm run build`;
  expect zero failures. `tsc` is the parity check: a copy key missing in
  either language fails the `Table` type; the descriptor test above fails on
  an empty string.

- [x] Commit: `git add mur-hub-gui/ui/src/components/SubscriptionProviderPanel.tsx
  mur-hub-gui/ui/src/components/ModelLibrary.tsx
  mur-hub-gui/ui/src/components/modelLibraryHelpers.ts
  mur-hub-gui/ui/src/components/modelLibraryHelpers.test.ts
  mur-hub-gui/ui/src/components/chatgptSubscription.ts
  mur-hub-gui/ui/src/components/chatgptSubscription.test.ts
  mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts`
  (plus the removal of `ChatGPTSubscriptionPanel.tsx` — `git mv` staged it),
  then `git commit -m "feat(hub): descriptor-driven subscription panel with Claude"`.

---

## Task 7 — Doctor checks and documentation

**Interfaces**

Consumes: `mur_core::cmd::model_doctor::{audit, Finding, is_local_endpoint}`;
`ModelEntry { provider, base_url, secret }`.

Produces: two additional `Finding::warn` classes from `audit`, subjects are
registry keys:

- `anthropic` + loopback `base_url` + no `secret` → warn suggesting `provider: claude`.
- `claude` + (non-loopback `base_url`, or path not `/v1`, or `secret` present) → warn that the runtime refuses the entry.

**Steps**

- [x] In `mur-core/src/cmd/model_doctor.rs` tests, add:

```rust
    #[test]
    fn subscription_entries_are_checked_against_the_loopback_contract() {
        let mut reg = reg_with(&[
            ("gw_anthropic", "anthropic", "claude-opus-5", Some("http://127.0.0.1:8088")),
            ("api_anthropic", "anthropic", "claude-opus-5", None),
            ("good_claude", "claude", "claude-opus-5", Some("http://127.0.0.1:8088/v1")),
            ("remote_claude", "claude", "claude-opus-5", Some("https://api.anthropic.com/v1")),
            ("wrong_path_claude", "claude", "claude-opus-5", Some("http://127.0.0.1:8088")),
            ("secret_claude", "claude", "claude-opus-5", Some("http://127.0.0.1:8088/v1")),
        ]);
        reg.models.get_mut("secret_claude").unwrap().secret =
            Some(mur_common::secret::SecretRef::Env("ANTHROPIC_API_KEY".into()));
        let findings = audit(&reg, &[], None);
        let subjects = |needle: &str| -> Vec<String> {
            findings
                .iter()
                .filter(|f| f.level == Level::Warn && f.detail.contains(needle))
                .map(|f| f.subject.clone())
                .collect()
        };
        assert_eq!(subjects("provider: claude"), vec!["gw_anthropic"]);
        let mut refused = subjects("refuses");
        refused.sort();
        assert_eq!(refused, vec!["remote_claude", "secret_claude", "wrong_path_claude"]);
        assert!(!findings.iter().any(|f| f.subject == "good_claude" || f.subject == "api_anthropic"));
    }
```

  (`Level` already derives `PartialEq`; `is_local_endpoint` already treats
  `127.0.0.1`, `localhost`, and `::1` as local.)

- [x] Run `cargo test -p mur-core --lib model_doctor::tests::subscription_entries_are_checked_against_the_loopback_contract`
  (with `ORT_STRATEGY=download` and `MUR_WEB_DIST` set as this repo's build
  notes require) and watch it fail (no findings).

- [x] In `audit`, after the plaintext-secret loop (section 0) and before
  section 1, add:

```rust
    // 0b. Subscription entries and the loopback contract.
    //
    //    `provider: anthropic` pointed at the local gateway *works* — the
    //    gateway attaches the Claude Code token to an authless request — but
    //    nothing stops a later `base_url` edit from landing the same entry on
    //    API billing. `provider: claude` is the same route with that edit
    //    refused at startup. Warn-only: the entry is not broken, it is
    //    unlabelled. The claude checks name what the runtime will refuse, so
    //    the user learns it here rather than from a failed agent start.
    for (key, e) in &reg.models {
        let loopback = is_local_endpoint(e.base_url.as_deref());
        match e.provider.as_str() {
            "anthropic" if loopback && e.secret.is_none() => out.push(Finding::warn(
                key.clone(),
                "rides a Claude subscription through the local gateway. `provider: claude` \
                 says so explicitly, labels it `billing: subscription`, and refuses a remote \
                 host or a secret — see docs/model-gateway.md",
            )),
            "claude" => {
                let path_ok = e
                    .base_url
                    .as_deref()
                    .and_then(|b| reqwest::Url::parse(b).ok())
                    .is_some_and(|u| u.path().trim_end_matches('/') == "/v1");
                if !loopback || !path_ok {
                    out.push(Finding::warn(
                        key.clone(),
                        "`provider: claude` must point at the loopback gateway \
                         `http://127.0.0.1:<port>/v1`; the runtime refuses to start this entry",
                    ));
                }
                if e.secret.is_some() {
                    out.push(Finding::warn(
                        key.clone(),
                        "`provider: claude` takes no `secret` — the gateway holds the Claude \
                         Code login; the runtime refuses to start this entry",
                    ));
                }
            }
            _ => {}
        }
    }
```

- [x] Run the focused test, then `cargo nextest run -p mur-core model_doctor`
  and `cargo clippy -p mur-core --all-targets -- -D warnings`; expect zero
  failures/warnings. Then run the installed-style check against the real
  registry: `cargo run -q -- model doctor` from the worktree and confirm the
  new warnings (if any) read sensibly for this machine's `~/.mur/models.yaml`
  — this check exists because the last doctor rule was caught printing
  unusable advice only on real data.

- [x] `docs/model-gateway.md`: add a `## Claude Subscription` section directly
  after the `## ChatGPT Subscription` section, with the same sub-headings
  (Setup; The fixed route and no-key behaviour; Subscription vs usage
  billing; Disconnect vs sign out) adapted: CLI is Claude Code
  (`npm install -g @anthropic-ai/claude-code`), sign-in is `claude auth login
  --claudeai`, the route is `http://127.0.0.1:8088/v1`, the entry is
  `provider: claude`, health field is `claudeCredential`, models come from
  the models.dev catalog, a Console login is not a subscription, and global
  sign-out affects every Claude Code session and IDE extension. Include the
  registry example:

```yaml
models:
  claude_opus_5:
    provider: claude
    model: claude-opus-5
    base_url: http://127.0.0.1:8088/v1
    tier: frontier
    billing: subscription
    catalog_verified: true
```

  and a short **Already using `provider: anthropic` with the gateway?**
  paragraph: it keeps working; `mur model doctor` points at the entries
  that could become `provider: claude`, and the change is the two lines
  `provider:` and `base_url` (append `/v1`), nothing else.

- [x] `README.md`: directly after the **ChatGPT Subscription, no API key.**
  paragraph add:

```markdown
**Claude Subscription, the same way.** The Model Library's **Claude Subscription** provider signs in through Claude Code (`claude auth login`), lists the models from the catalog, and writes `provider: claude` entries that can only reach the loopback gateway's `/v1` route — no `secret`, and a `base_url` edit to `api.anthropic.com` is refused at startup instead of quietly switching the bill. Entries you already point at the gateway as `provider: anthropic` keep working; `mur model doctor` shows which ones could carry the explicit label.
```

- [x] Commit: `git add mur-core/src/cmd/model_doctor.rs docs/model-gateway.md README.md`,
  then `git commit -m "feat(doctor): flag subscription entries off the loopback contract; docs"`.

---

## Task 8 — Cross-repo gates and end-to-end verification

**Steps**

- [x] Run the complete automated gates from the `mur` worktree:

```text
cargo nextest run -p mur-common -p mur-agent-runtime -p mur-core
cargo fmt --check
cargo clippy -p mur-common -p mur-agent-runtime -p mur-core --all-targets -- -D warnings
cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --all-targets -- -D warnings
cd mur-hub-gui/ui && npm test && npm run lint && npm run build
```

  Expect zero failures and zero clippy warnings. (`cargo nextest`, not
  `cargo test`, for `mur-common`: one of its secret-cache tests races under
  in-process parallelism — a pre-existing flake documented in the ChatGPT
  plan's handoff.)

- [x] Run the complete gateway gates from the gateway worktree:

```text
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

- [x] Merge the gateway PR from Task 1 and update the local gateway
  (`brew upgrade mur-model-gateway` or rebuild + `mur-model-gateway install`
  + relaunch). Confirm with
  `curl -s http://127.0.0.1:8088/__mur/health` that `claudeCredential` is
  present.

- [x] Acceptance, with a real Claude subscription, 2026-09-03. **Verified
  live:** `claude auth status --json` returns exactly the parsed shape
  (`loggedIn: true`, `authMethod: "claude.ai"`, `email`); the rebuilt gateway
  reports `claudeCredential: "oauth"`; a disposable agent on a
  `provider: claude` entry completed a turn, and the gateway logged
  `path=/v1/messages provider=Anthropic disguise=true status=200` 3 ms before
  the turn's `completedAt` (no `?beta=true`, which distinguishes it from the
  machine's own Claude Code traffic); flipping that entry's `base_url` to
  `https://api.anthropic.com/v1` produced `rejected: scheme must be http
  (loopback only)` at build time, so the request never left the machine; the
  old installed runtime lacks the `claude` arm and the new one has it
  (binary-level negative control); `mur model doctor` on the real registry
  flagged nothing new and left the valid entry alone; the token-name scan of
  `~/.mur/models.yaml`, agent profiles and the gateway logs found nothing.
  **Not driven live:** the Hub panel steps (sign-in card, model list, add,
  Disconnect). The Hub had to be rebuilt to contain the panel, and
  computer-use wedged on its known "通知中心" frontmost-detector desync
  (`gotcha_computeruse_frontmost_wedged_notification_center`), which only a
  human click clears. The panel's logic is covered by the UI tests; its live
  run is the one open item.
  Original sequence, for a later manual pass:
  1. Hub → Model Library → Add Provider → **Claude Subscription**. With
     Claude Code signed out, the panel shows the sign-in card; **Sign in
     with Claude** completes in the browser and the panel shows the email.
  2. Gateway card shows ready (`claudeCredential: oauth`). Add one model.
  3. Assign it to a disposable agent; send one turn. The gateway log shows
     `provider=Anthropic`, and the request carried no `x-api-key`
     (`grep -c 'x-api-key' ~/Library/Logs/mur-model-gateway/proxy.log` does
     not grow).
  4. Edit that entry's `base_url` to `https://api.anthropic.com/v1`; the
     agent refuses to start with the `rejected` message; revert.
  5. **Disconnect MUR**; `claude auth status` still reports `loggedIn: true`.
  6. **Sign out of Claude** (confirm); the remaining entries show unhealthy;
     sign back in.

- [x] Search generated configuration and logs by key names, not token values:

```text
rg -n "sk-ant-oat|accessToken|refreshToken|ANTHROPIC_API_KEY" \
  ~/.mur/models.yaml ~/.mur/agents/*/profile.yaml \
  ~/Library/Logs/mur-model-gateway 2>/dev/null
```

  Expected: no matches.

- [x] Update the design status to `Implemented` only after the real
  end-to-end turn succeeds. Commit:
  `git add docs/superpowers/specs/2026-09-03-mur-hub-claude-subscription-design.md`
  and the ticked plan, then
  `git commit -m "docs(hub): mark Claude subscription design implemented"`.

- [x] Open the `mur` PR against `main`; after merge, update the docs site
  and product page in `mur-server` (a `## Claude Subscription` section on
  `docs-content/model-gateway.md` and one feature card) via the
  `update-docs` skill — a separate PR that publishes to app.mur.run and is
  never auto-merged.

- [x] Run `git status --short` in both worktrees. Expected: empty. Record
  the exact successful commands and commit IDs in the handoff.

## Spec coverage map

| Design requirement | Tasks |
|---|---|
| Distinct `claude` provider, loopback-only `/v1`, no secret | 3, 7 |
| Authless header **absent**, never empty | 2 |
| `claudeCredential` in gateway health (kind only) | 1, 4 |
| `claude auth status` as account source; Console ≠ subscription | 5, 6 |
| Wrapped login/logout, confirmation on logout | 5, 6 |
| Catalog-sourced model list; unverified-id fallback | 5, 6 |
| One panel for both providers (descriptor) | 6 |
| Billing labels / no automatic paid fallback | (unchanged from mur#1154) 8 acceptance |
| Existing `provider: anthropic` untouched; doctor hint | 3, 7 |
| Disconnect vs global sign-out | 5, 6, 7 |
| Docs (`model-gateway.md`, README, docs site, product page) | 7, 8 |
