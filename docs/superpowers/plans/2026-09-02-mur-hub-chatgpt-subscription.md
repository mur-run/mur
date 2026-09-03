# Plan — MUR Hub ChatGPT Subscription provider

> **Execute with `mur-executing-plans`.** Work task-by-task in order. Task 1
> is in `/Volumes/Firecuda4tb/Projects/mur-model-gateway`; Tasks 2–11 are in
> `/Volumes/Firecuda4tb/Projects/mur`. Do not begin a later task until the
> preceding task's tests and review gate pass.

Design:
`docs/superpowers/specs/2026-09-02-mur-hub-chatgpt-subscription-design.md`.

## Goal

Let MUR Hub connect a user's Codex-managed ChatGPT subscription, discover the
account's available models, and run agents through the local model gateway
without copying a token or silently falling into OpenAI Platform billing.

## Architecture

Hub uses a short-lived `codex app-server` stdio session as its control plane
for `account/read` and `model/list`, and the official `codex login`/`logout`
commands for credential lifecycle. Registry entries use the distinct wire
provider `codex`; runtime sends authless OpenAI Chat Completions traffic only
to the loopback gateway, which owns OAuth attachment, refresh, and Responses
translation.

## Tech stack

Rust 2024 (`mur-common`, `mur-agent-runtime`, Tauri 2, Axum/Reqwest), React 18
+ TypeScript + Vitest, JSONL app-server protocol, YAML model registry, and the
Rust `mur-model-gateway` service.

## Global Constraints

Copied from the approved design. Every task implicitly includes all of them.

- Codex owns authentication and credential storage.
- MUR Hub owns the connection experience, account/model status, and model registration.
- `codex app-server` is the control plane for account and model information.
- `mur-model-gateway` remains the inference data plane.
- MUR runtime reuses the OpenAI Chat Completions wire format but sends no API key to the loopback Codex route.
- Hub and runtime never read, parse, log, or serialize `~/.codex/auth.json`.
- No UI, command result, diagnostic, analytics event, or registry field contains an access or refresh token.
- `provider` is exactly `codex`; `secret` is absent.
- The loopback restriction is a safety property, not merely a Hub validation.
- Existing `provider: openai` behavior remains unchanged and continues to require an API key.
- MUR never inserts a usage-billed OpenAI model into a ChatGPT model's fallback chain automatically.
- A 429 does not grant permission to add or select a paid fallback.
- Disconnecting MUR does not modify the shared Codex login.
- Signing out of ChatGPT requires confirmation because it affects Codex CLI and IDE clients.
- Registry discovery/add is non-destructive: an existing alias always wins.
- Repo rules: no hardcoded secrets, single source file ≤ 800 lines, `cargo fmt` clean, `cargo clippy --all-targets -- -D warnings` clean, UI strings in both English and Traditional Chinese.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `/Volumes/Firecuda4tb/Projects/mur-model-gateway/src/codex.rs` | non-secret compiled-hook and credential-mode diagnostics | 1 |
| `/Volumes/Firecuda4tb/Projects/mur-model-gateway/src/lib.rs` | loopback health/readiness endpoint | 1 |
| `/Volumes/Firecuda4tb/Projects/mur-model-gateway/tests/health.rs` | gateway health contract tests | 1 |
| `mur-common/src/model.rs` | billing and catalog-verification registry metadata | 2 |
| `mur-agent-runtime/src/llm/openai.rs` | shared optional-auth Chat Completions transport | 3 |
| `mur-agent-runtime/src/llm/codex.rs` | loopback-only authless Codex client | 4 |
| `mur-agent-runtime/src/llm/mod.rs` | export `CodexClient` | 4 |
| `mur-agent-runtime/src/llm/client_builder.rs` | build `provider: codex` without secrets | 4 |
| `mur-hub-gui/src-tauri/Cargo.toml` | enable Tokio process/io features | 5 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/app_server.rs` | bounded JSONL app-server client | 5 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/mod.rs` | typed Tauri views and orchestration | 5–7 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/process.rs` | login/logout and gateway process lifecycle | 6 |
| `mur-hub-gui/src-tauri/src/lib.rs` | module and Tauri command registration | 5–7 |
| `mur-hub-gui/src-tauri/src/models_admin.rs` | subscription model registry add/disconnect | 7 |
| `mur-hub-gui/ui/src/components/chatgptSubscription.ts` | pure state reducer, labels, DTOs | 8 |
| `mur-hub-gui/ui/src/components/chatgptSubscription.test.ts` | UI state/billing behavior tests | 8 |
| `mur-hub-gui/ui/src/components/ChatGPTSubscriptionPanel.tsx` | dedicated provider panel | 9 |
| `mur-hub-gui/ui/src/components/ModelLibrary.tsx` | route provider card to dedicated panel | 9 |
| `mur-hub-gui/ui/src/components/modelLibraryHelpers.ts` | subscription provider descriptor | 9 |
| `mur-hub-gui/ui/src/components/settings/FallbackChainEditor.tsx` | billing labels and paid-fallback warning | 10 |
| `mur-hub-gui/ui/src/components/settings/FallbackChainEditor.test.ts` | paid-fallback warning tests | 10 |
| `mur-hub-gui/ui/src/components/modelPicker.ts` | carry billing metadata to UI | 10 |
| `mur-hub-gui/src-tauri/src/detail.rs` | expose registry billing metadata to Hub | 10 |
| `mur-hub-gui/ui/src/i18n/en.ts` | English strings | 9–10 |
| `mur-hub-gui/ui/src/i18n/zh-TW.ts` | Traditional Chinese strings | 9–10 |
| `mur-hub-gui/ui/src/styles.css` | provider state/card styling | 9 |
| `docs/model-gateway.md` | user-facing setup, billing, disconnect/logout | 11 |

---

## Task 1 — Add a non-billable gateway readiness contract

**Repository:** `/Volumes/Firecuda4tb/Projects/mur-model-gateway`

**Interfaces**

Consumes:

```rust
pub fn codex::default_auth_path() -> Option<PathBuf>
pub fn codex::read_credential(path: &Path) -> Option<CodexCredential>
```

Produces:

```rust
pub fn codex::hook_compiled() -> bool
GET /__mur/health
// 200 JSON:
// {"status":"ok","codexHook":true,"codexCredential":"chatgpt","compression":true}
```

`codexCredential` is exactly `chatgpt`, `apikey`, or `missing`; it never
contains account IDs or token material.

**Steps**

- [x] Create `tests/health.rs` with an Axum test server and these assertions:

```rust
#[tokio::test]
async fn health_is_local_and_non_secret() {
    let state = AppState::new("http://127.0.0.1:9", "http://127.0.0.1:9",
        "http://127.0.0.1:9", TokenSource::Disabled).unwrap()
        .with_token_source_codex(TokenSource::Disabled);
    let app = build_router(state);
    let response = app.oneshot(
        Request::builder().uri("/__mur/health").body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ok");
    assert!(json.get("codexHook").unwrap().is_boolean());
    assert_eq!(json["codexCredential"], "missing");
    assert!(json.get("compression").unwrap().is_boolean());
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!raw.contains("access_token"));
    assert!(!raw.contains("refresh_token"));
}
```

- [x] Run `cargo test --test health` and watch it fail because the route does
  not exist.

- [x] Add both cfg arms in `src/codex.rs`:

```rust
#[cfg(has_codex_hook)]
pub const fn hook_compiled() -> bool { true }

#[cfg(not(has_codex_hook))]
pub const fn hook_compiled() -> bool { false }
```

- [x] Add a private serializable response and handler in `src/lib.rs`. The
  handler derives credential mode only when `token_source_codex` is
  `TokenSource::Codex`; every other source reports `missing`:

```rust
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayHealth {
    status: &'static str,
    codex_hook: bool,
    codex_credential: &'static str,
    compression: bool,
}

async fn health(State(state): State<AppState>) -> axum::Json<GatewayHealth> {
    let mode = match &state.token_source_codex {
        TokenSource::Codex(path) => match codex::read_credential(path) {
            Some(codex::CodexCredential::OAuth { .. }) => "chatgpt",
            Some(codex::CodexCredential::ApiKey { .. }) => "apikey",
            None => "missing",
        },
        _ => "missing",
    };
    axum::Json(GatewayHealth {
        status: "ok",
        codex_hook: codex::hook_compiled(),
        codex_credential: mode,
        compression: state.compress,
    })
}
```

- [x] Register `.route("/__mur/health", axum::routing::get(health))` before
  the wildcard route in `build_router`.

- [x] Run `cargo test --test health` and expect one passing test. Run
  `cargo test`, `cargo fmt --check`, and
  `cargo clippy --all-targets -- -D warnings`; expect zero failures/warnings.

- [x] Commit in the gateway repository:
  `git add src/codex.rs src/lib.rs tests/health.rs`, then
  `git commit -m "feat(codex): expose non-secret readiness"`.

---

## Task 2 — Add registry billing and verification metadata

**Interfaces**

Consumes: existing `mur_common::model::ModelEntry` serde contract.

Produces:

```rust
#[serde(rename_all = "snake_case")]
pub enum BillingMode { Subscription, UsageBilled, Local }

pub struct ModelEntry {
    pub billing: Option<BillingMode>,
    pub catalog_verified: Option<bool>,
    // existing fields unchanged
}
```

**Steps**

- [x] Add a serde round-trip test in `mur-common/src/model.rs`:

```rust
#[test]
fn subscription_metadata_round_trips_without_a_secret() {
    let yaml = r#"schema_version: 1
models:
  chatgpt_sol:
    provider: codex
    model: gpt-5.6-sol
    base_url: http://127.0.0.1:8088/codex/v1
    tier: frontier
    billing: subscription
    catalog_verified: true
"#;
    let reg: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    let entry = &reg.models["chatgpt_sol"];
    assert_eq!(entry.billing, Some(BillingMode::Subscription));
    assert_eq!(entry.catalog_verified, Some(true));
    assert!(entry.secret.is_none());
    let out = serde_yaml_ng::to_string(&reg).unwrap();
    assert!(out.contains("billing: subscription"));
}
```

- [x] Run
  `cargo test -p mur-common --lib subscription_metadata_round_trips_without_a_secret`
  and watch it fail to compile.

- [x] Add `BillingMode` above `ModelEntry` and add both optional fields with
  `#[serde(default, skip_serializing_if = "Option::is_none")]`. Derive
  `Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq`.

- [x] Add a regression test that parses and reserializes an old entry without
  either field and asserts both are `None`.

- [x] Run `cargo test -p mur-common --lib model::`, then
  `cargo clippy -p mur-common --all-targets -- -D warnings`; expect zero
  failures/warnings.

- [x] Commit: `git add mur-common/src/model.rs`, then
  `git commit -m "feat(models): classify model billing source"`.

---

## Task 3 — Make OpenAI transport authentication explicit

**Interfaces**

Consumes: existing `OpenAiClient` request/response conversion.

Produces:

```rust
#[derive(Clone)]
enum OpenAiAuth { Bearer(String), None }

impl OpenAiClient {
    pub(crate) fn authless_with_http(
        base_url: String,
        model: String,
        http: reqwest::Client,
    ) -> Self;
}
```

All existing public constructors continue to create `OpenAiAuth::Bearer`.

**Steps**

- [x] Add a mock-server test beside the existing OpenAI client tests that
  constructs `authless_with_http`, calls `generate`, and asserts the captured
  request has neither `authorization` nor `x-api-key`.

- [x] Run the focused test and watch it fail because the constructor is
  absent.

- [x] Replace `api_key: String` in `OpenAiClient` with `auth: OpenAiAuth`.
  Route every existing constructor through `OpenAiAuth::Bearer`; add the
  authless constructor exactly as declared above.

- [x] Add one helper and use it on both streaming and non-streaming paths:

```rust
fn apply_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match &self.auth {
        OpenAiAuth::Bearer(key) => request.bearer_auth(key),
        OpenAiAuth::None => request,
    }
}
```

  Replace both direct `.bearer_auth(&self.api_key)` calls with
  `self.apply_auth(self.http.post(url))` before `.json(&body)`.

- [x] Add a regression assertion that `OpenAiClient::new` still sends
  `Authorization: Bearer test-key`.

- [x] Run all OpenAI client tests and
  `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings`; expect
  zero failures/warnings.

- [x] Commit: `git add mur-agent-runtime/src/llm/openai.rs`, then
  `git commit -m "refactor(openai): make transport auth explicit"`.

---

## Task 4 — Add the loopback-only `CodexClient` and runtime factory branch

**Interfaces**

Consumes:

```rust
OpenAiClient::authless_with_http(base_url, model, http)
ModelEntry { provider, model, base_url, secret, .. }
```

Produces:

```rust
pub struct CodexClient { inner: OpenAiClient }
impl CodexClient {
    pub fn with_http_client(
        base_url: String,
        model: String,
        http: reqwest::Client,
    ) -> Result<Self, LlmError>;
}
```

**Steps**

- [x] Create `mur-agent-runtime/src/llm/codex.rs` with table-driven URL tests:

```rust
#[test]
fn accepts_only_loopback_codex_base_urls() {
    for ok in [
        "http://127.0.0.1:8088/codex/v1",
        "http://localhost:8088/codex/v1",
        "http://[::1]:8088/codex/v1",
    ] {
        assert!(validate_codex_base_url(ok).is_ok(), "{ok}");
    }
    for bad in [
        "https://api.openai.com/v1",
        "http://127.0.0.1:8088/v1",
        "http://localhost.evil.test:8088/codex/v1",
        "http://user@127.0.0.1:8088/codex/v1",
        "http://192.168.1.2:8088/codex/v1",
    ] {
        assert!(validate_codex_base_url(bad).is_err(), "{bad}");
    }
}
```

- [x] Run the focused test and watch it fail because the module/function is
  absent.

- [x] Implement `validate_codex_base_url(&str) -> Result<reqwest::Url,
  LlmError>`: scheme must be `http`, username/password empty, host exactly
  `localhost` or an IP for which `is_loopback()` is true, explicit port is
  required, query/fragment absent, and normalized path exactly `/codex/v1`.

- [x] Implement `CodexClient::with_http_client`, then delegate both trait
  methods without changing request semantics:

```rust
#[async_trait::async_trait]
impl LlmClient for CodexClient {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.inner.generate(req).await
    }
    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        self.inner.generate_stream(req, sink).await
    }
}
```

- [x] Export the module/type from `llm/mod.rs`. Add a `"codex"` arm to
  `client_builder.rs`: reject `entry.secret.is_some()`, require `base_url`,
  and construct `CodexClient` with the guarded HTTP client. Do not inspect
  `OPENAI_API_KEY` or the agent keychain.

- [x] Add factory tests for: valid secret-free Codex entry succeeds; missing
  URL fails; secret present fails; remote URL fails; an OpenAI entry without a
  key still fails as before.

- [x] Run `cargo test -p mur-agent-runtime --lib llm::`, then formatting and
  clippy for the crate; expect zero failures/warnings.

- [x] Commit: `git add mur-agent-runtime/src/llm/codex.rs
  mur-agent-runtime/src/llm/mod.rs mur-agent-runtime/src/llm/client_builder.rs`,
  then
  `git commit -m "feat(runtime): add loopback ChatGPT subscription provider"`.

---

## Task 5 — Implement the bounded Codex app-server control-plane adapter

**Interfaces**

Consumes: `codex app-server` JSONL protocol: `initialize`, `initialized`,
`account/read`, and paginated `model/list`.

Produces:

```rust
pub struct ChatGptAccountView {
    pub cli_present: bool,
    pub logged_in: bool,
    pub auth_mode: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}
pub struct ChatGptModelView {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
    pub reasoning_efforts: Vec<String>,
    pub input_modalities: Vec<String>,
}
pub async fn read_account(codex: &Path) -> Result<ChatGptAccountView, ControlError>;
pub async fn list_models(codex: &Path) -> Result<Vec<ChatGptModelView>, ControlError>;
```

**Steps**

- [x] Enable Tokio `process` and `io-util` features in the Hub Tauri crate.

- [x] Create `chatgpt_subscription/app_server.rs`. Define a private
  `JsonlSession` that spawns `codex app-server --listen stdio://` with null
  stdin inheritance, piped stdin/stdout, and piped-but-bounded stderr. Every
  request uses a monotonically increasing `u64` ID and a 15-second timeout.

- [x] Add fixture-driven tests using a temporary executable script that reads
  JSONL and answers the exact request IDs. Assert the first two writes are:

```json
{"method":"initialize","id":1,"params":{"clientInfo":{"name":"mur_hub","title":"MUR Hub","version":"0.1.0"}}}
{"method":"initialized","params":{}}
```

  The test must also prove notifications without `id` are skipped while
  waiting for a response, an `error` response becomes `ControlError::Rpc`, and
  EOF/timeout kills and reaps the child.

- [x] Implement initialization. Do not set `experimentalApi`; all required
  methods are stable.

- [x] Implement `account/read` with
  `{"refreshToken":false}`. Treat only `account.type == "chatgpt"` as the
  subscription-connected state. `apiKey` must render as not connected to this
  provider, never as ChatGPT subscription.

- [x] Implement `model/list` with `limit: 100` and `includeHidden: false`.
  Follow `nextCursor` until null, deduplicate by `model` (fall back to `id`),
  default absent `inputModalities` to `["text", "image"]`, and retain only
  display-safe fields listed in `ChatGptModelView`.

- [x] Register Tauri commands `chatgpt_account_read` and
  `chatgpt_models_list` in `src/lib.rs`. Resolve the `codex` executable once
  per call using the same PATH search discipline as existing CLI-tool code;
  return a typed `cli_present: false` view when it cannot be found.

- [x] Run
  `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml chatgpt_subscription`
  and clippy on that manifest; expect zero failures/warnings.

- [x] Commit: `git add mur-hub-gui/src-tauri/Cargo.toml
  mur-hub-gui/src-tauri/src/chatgpt_subscription
  mur-hub-gui/src-tauri/src/lib.rs`, then
  `git commit -m "feat(hub): read ChatGPT account and model catalog"`.

---

## Task 6 — Add login/logout and gateway lifecycle commands

**Interfaces**

Consumes: Task 5 account adapter and installed `codex` /
`mur-model-gateway` executables.

Produces:

```rust
pub struct LoginResult { pub authenticated: bool, pub error: Option<String> }
pub struct GatewayStatusView {
    pub installed: bool,
    pub running: bool,
    pub codex_hook: bool,
    pub credential_mode: Option<String>,
    pub compression: bool,
}
#[tauri::command] async fn chatgpt_login() -> Result<LoginResult, String>;
#[tauri::command] async fn chatgpt_logout(confirmed: bool) -> Result<(), String>;
#[tauri::command] async fn chatgpt_gateway_status() -> Result<GatewayStatusView, String>;
#[tauri::command] async fn chatgpt_gateway_install(consented: bool) -> Result<GatewayStatusView, String>;
```

**Steps**

- [x] Create `chatgpt_subscription/process.rs` and tests around injected
  executable paths. `chatgpt_login` runs `codex login`, captures at most 32 KiB
  of combined output, enforces a five-minute timeout, reaps on cancellation,
  and then calls `account/read`; exit code zero alone is not success.

- [x] Serialize login attempts with a process-global async mutex so two Hub
  windows cannot launch two browser flows.

- [x] Make `chatgpt_logout(false)` return
  `confirmation required: signing out affects Codex CLI and IDE`. Only the
  confirmed arm runs `codex logout`; afterwards require `account/read` to
  report no ChatGPT account.

- [x] Implement gateway status in two layers: locate the binary/service, then
  call `GET http://127.0.0.1:8088/__mur/health` with a two-second timeout.
  `running` means a valid health response, not merely a service file.

- [x] Parse health only into the Task 6 view. Reject a response whose
  `codexCredential` is `apikey` as not ready for subscription use.

- [x] `chatgpt_gateway_install(false)` must fail before spawning anything.
  The consented arm runs:

```text
mur-model-gateway install --token-source-codex codex
```

  It then polls health for at most ten seconds. Preserve existing compression
  configuration: if the installed service already reports compression on,
  append `--compress`; otherwise do not change it.

- [x] Register all four commands. Tests must prove no install/logout process
  starts without the boolean confirmation and that child output is bounded and
  control characters are stripped before returning diagnostics.

- [x] Run Hub Tauri tests and clippy; expect zero failures/warnings.

- [x] Commit: `git add mur-hub-gui/src-tauri/src/chatgpt_subscription
  mur-hub-gui/src-tauri/src/lib.rs`, then
  `git commit -m "feat(hub): manage ChatGPT login and gateway readiness"`.

---

## Task 7 — Add subscription-specific registry commands

**Interfaces**

Consumes:

```rust
BillingMode::Subscription
ChatGptModelView
const CHATGPT_GATEWAY_BASE: &str = "http://127.0.0.1:8088/codex/v1";
```

Produces:

```rust
pub struct ChatGptModelPick { pub model: String, pub alias: String, pub verified: bool }
#[tauri::command]
pub fn chatgpt_models_add(picks: Vec<ChatGptModelPick>) -> Result<(), String>;
#[tauri::command]
pub fn chatgpt_disconnect() -> Result<u32, String>;
```

**Steps**

- [x] Add tests in `models_admin.rs` using `MUR_HOME` isolation. Adding a pick
  must produce exactly:

```rust
ModelEntry {
    provider: "codex".into(),
    model: pick.model,
    base_url: Some(CHATGPT_GATEWAY_BASE.into()),
    secret: None,
    tier: Some(RouteTier::Frontier),
    billing: Some(BillingMode::Subscription),
    catalog_verified: Some(pick.verified),
    ..Default::default()
}
```

- [x] Add alias validation using the existing model-alias rules; reject empty,
  path-like, control-character, or duplicate aliases. Existing entries are
  never overwritten even if they are not Codex entries.

- [x] Implement `chatgpt_models_add`; do not reuse generic `add_models`, because
  that function builds a SecretRef and maps UI vendor names onto wire
  protocols.

- [x] Implement `chatgpt_disconnect` as removal of entries for which both
  `provider == "codex"` and `billing == Some(Subscription)`. Return the count.
  Do not run `codex logout`, stop the gateway, or remove hand-authored Codex
  entries without subscription metadata.

- [x] Register both commands and run focused tests plus Hub Tauri clippy.

- [x] Commit: `git add mur-hub-gui/src-tauri/src/models_admin.rs
  mur-hub-gui/src-tauri/src/chatgpt_subscription/mod.rs
  mur-hub-gui/src-tauri/src/lib.rs`, then
  `git commit -m "feat(hub): register ChatGPT subscription models"`.

---

## Task 8 — Define the pure Hub state machine

**Interfaces**

Consumes the Task 5–7 command DTOs.

Produces:

```ts
export type ChatGPTPanelState =
  | { kind: "codex-missing" }
  | { kind: "logged-out" }
  | { kind: "login-in-progress" }
  | { kind: "account-unavailable"; message: string }
  | { kind: "gateway-missing"; account: ChatGPTAccount }
  | { kind: "gateway-stopped"; account: ChatGPTAccount }
  | { kind: "models-loading"; account: ChatGPTAccount }
  | { kind: "ready"; account: ChatGPTAccount; models: ChatGPTModel[] };

export function deriveChatGPTState(input: ChatGPTStateInput): ChatGPTPanelState;
export function billingLabel(mode?: BillingMode): "Subscription" | "Usage billed" | "Local" | "Unknown";
```

**Steps**

- [x] Create `chatgptSubscription.test.ts` with a table that proves the exact
  precedence: missing CLI → logged out → account error → gateway missing →
  gateway stopped → models loading → ready.

- [x] Add tests that API-key account mode never becomes subscription-ready;
  missing legacy billing metadata renders `Unknown`; and `subscription`,
  `usage_billed`, `local` map to distinct labels.

- [x] Run `npm test -- chatgptSubscription.test.ts` in `mur-hub-gui/ui` and
  watch it fail because the module is absent.

- [x] Implement the DTOs, reducer, and billing label as pure functions with no
  Tauri imports or React state.

- [x] Run the focused test, full `npm test`, and `npm run lint`; expect zero
  failures.

- [x] Commit: `git add mur-hub-gui/ui/src/components/chatgptSubscription.ts
  mur-hub-gui/ui/src/components/chatgptSubscription.test.ts`, then
  `git commit -m "feat(hub): model ChatGPT subscription connection states"`.

---

## Task 9 — Build the dedicated ChatGPT Subscription panel

**Interfaces**

Consumes: Task 8 state model and Tauri commands from Tasks 5–7.

Produces:

```tsx
export function ChatGPTSubscriptionPanel(props: {
  registryModels: ModelOption[];
  onModelsAdded(): void;
}): JSX.Element;
```

**Steps**

- [x] Add a distinct provider descriptor to `modelLibraryHelpers.ts` rather
  than `CLOUD_PRESETS`:

```ts
export const CHATGPT_SUBSCRIPTION = {
  key: "chatgpt-subscription",
  name: "ChatGPT Subscription",
  logo: "GPT",
  color: "#10A37F",
} as const;
```

- [x] Extend `PanelKind` in `ModelLibrary.tsx` with
  `{ kind: "chatgpt-subscription" }`; render the provider in **Add Provider**
  separately from OpenAI and route it to `ChatGPTSubscriptionPanel`.

- [x] Implement panel loading with one cancellation flag per effect. Fetch
  account and gateway status in parallel; fetch models only for a ChatGPT
  account. Never call generic `test_provider` or `/v1/models`.

- [x] Implement state-specific actions: login, retry account, consent modal
  then install gateway, start/repair gateway via install, retry models, add
  selected models, disconnect MUR, and separately confirmed global logout.

- [x] Model rows show display name, ID, default marker, input modalities, and
  supported reasoning efforts. Default selection is only the `is_default`
  model. Alias edits reuse the existing checklist alias behavior.

- [x] When `model/list` fails, reveal an **Advanced: add unverified model ID**
  field. Submitting it calls `chatgpt_models_add` with `verified: false` and a
  visible `Unverified` badge; no static model list is substituted.

- [x] Add all copy in `en.ts` and `zh-TW.ts`. Required phrases distinguish
  ChatGPT subscription from OpenAI API usage billing and explain that global
  logout affects Codex CLI/IDE.

- [x] Add focused CSS using existing `ml-*` tokens; do not create a second
  visual language. Preserve keyboard focus, field labels, `aria-live` error
  announcements, and disabled/loading states.

- [x] Run UI tests, lint, and `npm run build`; expect zero failures.

- [x] Commit: `git add mur-hub-gui/ui/src/components/ChatGPTSubscriptionPanel.tsx
  mur-hub-gui/ui/src/components/ModelLibrary.tsx
  mur-hub-gui/ui/src/components/modelLibraryHelpers.ts
  mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
  mur-hub-gui/ui/src/styles.css`, then
  `git commit -m "feat(hub): add ChatGPT subscription provider panel"`.

---

## Task 10 — Surface billing safety in model and fallback views

**Interfaces**

Consumes `ModelEntry.billing` through the existing `list_models` Tauri view.

Produces:

```ts
export interface ModelOption {
  // existing fields
  billing?: "subscription" | "usage_billed" | "local";
  catalog_verified?: boolean;
}

export function paidFallbackWarning(primary: ModelOption, fallback: ModelOption): string | null;
```

**Steps**

- [x] Extend the Rust `ModelOption`/detail view returned by `list_models` with
  both optional fields. Add a serialization test for old and new entries.

- [x] Extend the TypeScript type and render a billing badge in the model picker
  and fallback editor. Unknown remains `Unknown`, never `$0`.

- [x] Add `paidFallbackWarning` tests: subscription → usage-billed warns;
  subscription → local/subscription does not; unknown does not silently claim
  safety and produces a neutral “billing unknown” warning.

- [x] The editor may save a paid fallback only after the user's existing
  explicit selection. It must never insert one on 429 or while connecting the
  subscription provider.

- [x] Run Rust view tests, full UI tests/lint/build, and verify en/zh-TW key
  parity with the existing i18n test.

- [x] Commit: `git add mur-hub-gui/src-tauri/src/detail.rs
  mur-hub-gui/ui/src/components/modelPicker.ts
  mur-hub-gui/ui/src/components/settings/FallbackChainEditor.tsx
  mur-hub-gui/ui/src/components/settings/FallbackChainEditor.test.ts
  mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts`, then
  `git commit -m "feat(hub): label model billing and paid fallbacks"`.

---

## Task 11 — Cross-repo contract and end-to-end verification

**Interfaces**

Consumes all prior task outputs; produces no new runtime API.

**Steps**

- [x] Add `docs/model-gateway.md` sections for ChatGPT Subscription setup,
  fixed `http://127.0.0.1:8088/codex/v1` route, no-key behavior, billing
  distinction, 429 behavior, disconnect, and global logout. Link to official
  OpenAI auth/app-server docs; do not document credential JSON fields.

- [x] Run the complete automated gates from the MUR repository:

```text
cargo test -p mur-common -p mur-agent-runtime
cargo fmt --check
cargo clippy -p mur-common -p mur-agent-runtime --all-targets -- -D warnings
cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --all-targets -- -D warnings
cd mur-hub-gui/ui && npm test && npm run lint && npm run build
```

  Expect zero failures and zero clippy warnings.

- [x] Run the complete gateway gates from the gateway repository:

```text
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

  Expect zero failures and zero warnings.

- [x] With a test ChatGPT account, perform the approved acceptance sequence:
  login from Hub; verify account/plan and model list; install/start gateway;
  register one model; assign it to a disposable agent; send one turn; confirm
  the gateway log reports `provider=Codex`; simulate 429 and confirm no paid
  fallback; disconnect MUR and confirm `codex login status` remains logged in;
  finally confirm global logout and verify retained entries become unhealthy.

- [x] Search generated configuration and logs by key names, not token values:

```text
rg -n "access_token|refresh_token|OPENAI_API_KEY" \
  ~/.mur/models.yaml ~/.mur/agents/*/profile.yaml \
  ~/Library/Logs/mur-model-gateway 2>/dev/null
```

  Expected: no token/key material in MUR registry, agent profiles, or gateway
  logs. A literal field name in a diagnostic fixture is acceptable only inside
  the source test tree, never user state.

- [x] Update the design status to `Implemented` only after the real end-to-end
  turn succeeds. Commit documentation and any test-only corrections:
  `git add docs/model-gateway.md
  docs/superpowers/specs/2026-09-02-mur-hub-chatgpt-subscription-design.md`,
  then `git commit -m "docs(hub): document ChatGPT subscription setup"`.

- [x] Run `git status --short` in both repositories. Expected: empty. Record
  the exact successful commands and commit IDs in the handoff.

## Spec coverage map

| Design requirement | Tasks |
|---|---|
| Dedicated `codex` provider | 2, 4, 7, 9 |
| Codex-owned login; no token in Hub | 5, 6, 9, 11 |
| app-server account/model control plane | 5, 8, 9 |
| gateway inference plane and readiness | 1, 4, 6, 11 |
| authless loopback-only runtime | 3, 4 |
| non-destructive registry entries | 2, 7 |
| separate disconnect and logout | 6, 7, 9 |
| 401/403/429 behavior | 1, 9, 10, 11 |
| billing labels and no implicit paid fallback | 2, 8, 10, 11 |
| unverified manual model fallback | 2, 7, 9 |
| existing OpenAI behavior unchanged | 3, 4, 10, 11 |
| security, redaction, and atomic credential ownership | 1, 4–6, 11 |
| English and Traditional Chinese UI | 9, 10 |

No requirement is deferred and no task depends on an interface that an earlier
task does not produce.
