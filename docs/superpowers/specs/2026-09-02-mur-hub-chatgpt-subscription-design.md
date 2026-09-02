# MUR Hub ChatGPT Subscription — Design

**Date:** 2026-09-02
**Status:** Approved (brainstorm 2026-09-02)
**Scope:** `mur-hub-gui`, `mur-agent-runtime`, and the existing
`mur-model-gateway` Codex contract

## Problem

MUR can already hand an interactive `/login chatgpt` flow to the Codex CLI,
and `mur-model-gateway` can use Codex's cached ChatGPT OAuth credential. The
Hub cannot connect that subscription path, however:

1. The Model Library exposes OpenAI only as a Platform API-key provider. Its
   API key field implies usage-based API billing and is not a safe place for a
   ChatGPT OAuth token.
2. Provider discovery assumes an OpenAI-compatible `GET /v1/models`. The
   gateway's translated Codex route does not expose that catalog.
3. The runtime's OpenAI client requires `OPENAI_API_KEY` and always sends an
   `Authorization` header. The gateway intentionally injects its stored Codex
   credential only when the inbound request has no client credential, so the
   two current contracts cannot compose.
4. ChatGPT subscription limits and OpenAI Platform billing are distinct, but
   the current UI has no way to communicate or preserve that distinction.

## Product Decision

Add an independent **ChatGPT Subscription** provider backed by the registry
wire name `codex`.

- Codex owns authentication and credential storage.
- MUR Hub owns the connection experience, account/model status, and model
  registration.
- `codex app-server` is the control plane for account and model information.
- `mur-model-gateway` remains the inference data plane.
- MUR runtime reuses the OpenAI Chat Completions wire format but sends no API
  key to the loopback Codex route.

This feature does not turn a ChatGPT subscription into a general OpenAI API
key. It provides access only through the Codex surfaces made available to the
signed-in ChatGPT account.

## Canonical Terms

- **ChatGPT Subscription provider:** the Hub provider shown to users. It uses
  Codex authentication and subscription limits, never OpenAI Platform API
  billing.
- **Codex provider:** the registry/runtime wire identity `provider: codex`.
  Avoid calling it an OpenAI provider even though its local request schema is
  OpenAI Chat Completions-compatible.
- **Control plane:** `codex app-server` operations used for account status and
  model discovery. It never carries MUR inference requests.
- **Inference plane:** the runtime-to-gateway-to-ChatGPT Codex request path.
- **Disconnect MUR:** disable or remove MUR registry use without changing the
  shared Codex login.
- **Sign out of ChatGPT:** run the global Codex logout flow, affecting other
  Codex clients that share the credential store.

## Goals

- Connect a ChatGPT subscription from MUR Hub without copying or displaying a
  token.
- Clearly distinguish subscription access from usage-billed OpenAI API access.
- List the models actually available to the signed-in ChatGPT account.
- Register selected models as dialable, secret-free `codex` entries.
- Safely install/start or diagnose the local gateway.
- Preserve the existing OpenAI API-key provider without behavior changes.
- Prevent silent fallback from a subscription to a usage-billed API.

## Non-goals

- No raw OAuth-token entry, inspection, export, or storage in Hub/MUR config.
- No replacement for the Codex login implementation or credential store.
- No general OpenAI Responses client in this phase; the gateway translation
  remains the compatibility boundary.
- No use of the Codex app-server thread, approval, or agent runtime as MUR's
  inference engine.
- No remote or shared-network Codex gateway. Subscription inference remains a
  single-user loopback feature.
- No automatic creation of an OpenAI Platform API key or billing account.

## Considered Approaches

### A. Independent `codex` provider (selected)

Hub uses app-server for control-plane data and the gateway for inference.
Runtime gains an authless loopback-only Codex client that reuses its OpenAI
Chat Completions codec.

**Why selected:** authentication, billing, model availability, and error
semantics stay explicit while existing OpenAI behavior remains untouched.

### B. Add an authentication toggle to the OpenAI provider

This minimizes visible provider count, but overloads one registry identity
with different billing systems, discovery protocols, upstreams, and secret
requirements. Error messages and fallback cost labels become ambiguous.

### C. Use Codex app-server for inference too

This offers deeper Codex integration but imports Codex threads, approvals,
and event semantics into MUR. It bypasses the already-working gateway
translation contract and is substantially larger than this feature.

## Architecture

```text
MUR Hub
├── ChatGPTSubscriptionPanel
│   ├── login/logout affordances
│   ├── account, workspace, and plan status
│   ├── available-model selector
│   └── gateway installation/health status
├── CodexAppServerClient
│   ├── account/read
│   └── model/list
├── CodexLoginRunner
│   ├── codex login
│   ├── codex login status
│   └── codex logout (explicit confirmation only)
└── Model Registry
    └── provider: codex; no secret

MUR Agent Runtime
└── CodexClient
    ├── reuses OpenAI Chat Completions request/stream codec
    ├── does not resolve OPENAI_API_KEY
    ├── sends no Authorization or x-api-key
    └── accepts only a loopback base URL

mur-model-gateway
├── reads the Codex-owned credential store
├── attaches ChatGPT OAuth and account headers
├── translates Chat Completions to Responses
├── translates Responses/SSE back to Chat Completions
└── refreshes on an eligible 401 and retries once
```

### Component boundaries

#### `ChatGPTSubscriptionPanel`

A dedicated Model Library panel, not a mode inside the existing OpenAI panel.
It renders a state machine instead of API-key/base-URL fields. It never accepts
or reveals a secret.

#### `CodexAppServerClient`

A Tauri-side adapter around a supervised `codex app-server` child. It exposes
typed, minimal operations to the UI:

- `account/read` for current authentication/account/plan state;
- paginated `model/list` with `includeHidden: false` for selectable models.

The adapter owns process startup, request IDs, timeouts, response parsing, and
shutdown. Raw app-server protocol messages do not cross into React.

#### `CodexLoginRunner`

Uses the existing safe terminal-handover behavior already designed for
`/login chatgpt`, or an equivalent Hub-owned child-process surface. Login is
always the official `codex login` flow. Hub observes completion and then asks
app-server for authoritative state; it never infers success from child exit
alone.

#### `CodexClient`

A dedicated runtime provider selected by `provider: codex`. It shares pure
message conversion and SSE parsing with `OpenAiClient`, but authentication is
not configurable: no SecretRef lookup and no authentication headers. Its base
URL policy is validated before the first request.

#### Gateway

The existing Codex behavior remains the protocol authority. Hub does not
duplicate OAuth parsing, request translation, or refresh logic. Any gateway
change needed by implementation should be limited to health/diagnostic
visibility, not a second login API.

## Hub State Model

The panel derives one primary state plus diagnostics:

```text
CodexMissing
LoggedOut
LoginInProgress
AccountUnavailable
GatewayMissing
GatewayStopped
ModelsLoading
Ready
```

Precedence is deterministic:

1. Missing Codex CLI blocks account and model operations.
2. Logged-out state offers login before gateway setup.
3. A valid account permits model discovery even if the gateway is absent.
4. Gateway readiness is required before models can be committed as ready to
   use.
5. A previously registered model may remain visible while unavailable, but it
   must carry an actionable unhealthy status.

The UI shows account/workspace/plan metadata returned by app-server. It never
shows token values, token fragments, or the credential file contents.

## Connection Flow

1. User opens **Models → Add Provider → ChatGPT Subscription**.
2. Hub locates the `codex` executable.
3. Hub reads account state through app-server.
4. If logged out, the user chooses **Sign in with ChatGPT** and Hub launches
   `codex login`.
5. On completion, Hub re-reads account state. A successful process exit without
   an authenticated account is still a failure.
6. Hub requests all visible pages from `model/list` and renders the returned
   model ID, display name, default flag, reasoning efforts, and modalities.
7. Hub checks gateway installation and health.
8. If installed but stopped, Hub may start it directly. If absent, Hub explains
   its local proxy role and requests confirmation before installation.
9. User selects one or more models and editable aliases.
10. Hub writes non-destructively to the model registry.

The official Codex app-server `model/list` result is authoritative because
availability can differ by ChatGPT account and workspace. A curated static
catalog is not used as the primary source. If discovery fails, an advanced
manual model-ID entry is permitted but marked **unverified** until a successful
inference or later catalog refresh.

## Registry Contract

Example:

```yaml
models:
  chatgpt_sol:
    provider: codex
    model: gpt-5.6-sol
    base_url: http://127.0.0.1:8088/codex/v1
    tier: frontier
    billing: subscription
```

Requirements:

- `provider` is exactly `codex`.
- `secret` is absent.
- `base_url` is generated by Hub, not copied from a vendor preset.
- `billing: subscription` is explicit metadata used by model/fallback UI. It
  must not be interpreted as a price guarantee or remaining-quota signal.
- Existing aliases are never overwritten during discovery/add.
- An unverified manual model ID is recorded in metadata so Hub can distinguish
  it from an app-server-confirmed model.

If adding `billing` or verification metadata to `ModelEntry` would force an
unrelated registry migration, implementation may place equivalent typed
metadata in the smallest backward-compatible optional fields. The user-visible
distinction and serialization round-trip are required; the precise field name
is an implementation-plan decision.

## Runtime Contract

The runtime factory adds a `codex` branch rather than routing the entry through
the existing `openai` branch.

The resulting client:

- requires `base_url`;
- accepts only `http://127.0.0.1:<port>/codex/v1`,
  `http://localhost:<port>/codex/v1`, or the IPv6 loopback equivalent;
- rejects userinfo, non-loopback resolved hosts, HTTPS-to-remote redirects,
  and path variants outside the Codex prefix;
- does not read agent keychain secrets or `OPENAI_API_KEY`;
- emits neither `Authorization` nor `x-api-key`;
- uses the existing HostGuard/egress policy in addition to the provider-level
  allowlist;
- preserves the current OpenAI-compatible tool, image, streaming, token-limit,
  and error handling wherever the gateway supports them.

The loopback restriction is a safety property, not merely a Hub validation.
Hand-edited registry files must be subject to the same runtime rejection.

## Inference Flow

```text
agent turn
  → CodexClient
  → POST http://127.0.0.1:8088/codex/v1/chat/completions
       (no client credential)
  → gateway resolves the Codex credential
  → gateway converts Chat Completions → Responses
  → POST https://chatgpt.com/backend-api/codex/responses
  → gateway converts Responses/SSE → Chat Completions
  → CodexClient
  → agent turn
```

The absence of client authentication headers is load-bearing: a non-empty
`Authorization` or `x-api-key` currently tells the gateway to preserve the
client's credential and skip subscription injection.

## Logout and Disconnect Semantics

Two separate actions are required:

- **Disconnect MUR:** disable/remove MUR's ChatGPT Subscription model entries.
  It does not modify Codex credentials and does not affect Codex CLI or IDE.
- **Sign out of ChatGPT:** warn that Codex clients share the credential, request
  confirmation, run `codex logout`, and then refresh Hub state. Model entries
  remain registered but unhealthy unless the user also disconnects them.

Deleting one model alias is neither disconnecting the provider nor signing out.

## Gateway Lifecycle

- Hub first performs a bounded loopback health check.
- An installed but stopped service gets a user-initiated **Start gateway**
  action; the connect flow may invoke it automatically after the user chose to
  connect.
- An absent gateway presents purpose, install location, loopback binding, and
  removal instructions before requesting installation approval.
- Installation is never silent and login success does not imply installation
  consent.
- Health reporting separates process/service status from actual Codex-route
  readiness.

A generic root HTTP status is insufficient for readiness. The implementation
plan should prefer a non-billable local diagnostic that proves the installed
binary has Codex support and can locate a credential without sending token
contents. It must not spend a model turn merely to paint a green badge.

## Error Handling

| Condition | Hub/runtime behavior |
|---|---|
| Codex CLI absent | Explain the dependency and provide the official install path; never offer token paste. |
| Login cancelled | Return to logged-out state; preserve no partial MUR configuration. |
| Account/workspace disallows Codex | Show the returned access problem; do not add ready entries. |
| `model/list` unavailable | Retry and offer advanced unverified model-ID entry. |
| Gateway absent | Offer explained, consent-gated installation. |
| Gateway stopped | Offer start and a copyable diagnostic summary. |
| Runtime sees a remote `codex` URL | Fail before sending any request. |
| Codex request carries client auth | Treat as a MUR configuration/programming error; do not silently change billing modes. |
| Upstream 401 | Gateway refreshes eligible OAuth once; persistent failure asks the user to sign in again. |
| Upstream 403 | Preserve the permission error; do not assume expiry. |
| Upstream 429/quota exhausted | Label as a ChatGPT subscription limit; do not fall through to Platform API automatically. |
| Model no longer available | Refresh `model/list`, mark the entry unavailable, and offer replacement. |
| app-server exits or times out | Tear down the child, retain the last non-secret state only for display, and offer retry. |

Errors exposed to the UI must be typed enough to select the proper recovery
action. Raw stderr may be included in an expandable diagnostic after redaction,
but it is not the primary user message.

## Fallback and Billing Safety

Model selectors and fallback editors label every candidate as one of:

- **Subscription**
- **Usage billed**
- **Local**

MUR never inserts a usage-billed OpenAI model into a ChatGPT model's fallback
chain automatically. Existing user-authored chains continue to run, but Hub
must surface their billing labels before saving changes. A 429 does not grant
permission to add or select a paid fallback.

## Security and Privacy

- Hub never reads, parses, logs, or serializes `~/.codex/auth.json`.
- No UI, command result, diagnostic, analytics event, or registry field contains
  an access or refresh token.
- Credential ownership stays with Codex. Gateway reads the credential because
  it must authenticate inference; runtime and Hub do not.
- `codex` authless behavior is loopback-only and enforced in the runtime.
- Gateway remains bound to loopback by default.
- Redirect handling must not allow an authless Codex client to escape the
  loopback boundary before reaching the gateway.
- Gateway refresh rotation remains atomic and preserves restrictive file
  permissions.
- Sign-out requires an explicit warning because it affects other Codex clients.

## Testing

### Unit tests

- Runtime factory builds `provider: codex` without a SecretRef or environment
  API key.
- CodexClient emits no authentication headers.
- Loopback URL acceptance covers IPv4, IPv6, and `localhost`; remote, userinfo,
  redirect, and malformed-path cases are rejected.
- App-server `account/read` and paginated `model/list` responses map into typed
  Hub views, including optional/older fields.
- Hub state precedence is deterministic for missing CLI, logged out, gateway
  states, discovery failure, and ready state.
- Disconnect, alias deletion, and global logout invoke distinct commands.
- Registry serialization round-trips subscription and verification metadata
  without a secret.

### Integration tests

- A fake app-server verifies request IDs, pagination, timeouts, child exit, and
  malformed-response handling.
- A fake gateway asserts the Chat Completions body and absence of
  `Authorization`/`x-api-key`.
- Adding selected models writes `provider: codex`, the fixed loopback URL, and
  no secret; existing aliases win.
- Existing `provider: openai` tests continue to require and send an API key.
- UI-to-Tauri tests cover login success, cancellation, gateway consent,
  discovery retry, manual unverified entry, disconnect, and logout confirmation.

### Gateway contract tests

- An authless translated request receives the gateway-owned OAuth headers.
- A client-supplied credential is not silently replaced.
- `/codex/v1/chat/completions` maps to the upstream `/responses` endpoint.
- Streaming and aggregated non-streaming replies map back correctly.
- Eligible 401 refreshes and retries at most once.
- API-key mode remains separate and cannot be selected by a subscription entry.
- Token material is absent from logs, Debug output, and error bodies.

### End-to-end acceptance

1. A fresh user signs in from Hub through the official Codex browser flow.
2. Hub reports the account/workspace/plan and lists account-available models.
3. User consents to gateway installation/start when needed.
4. User adds a model and creates or updates an agent to use its alias.
5. A real turn succeeds and gateway diagnostics identify `Provider::Codex`.
6. Registry, Hub logs, and agent secrets contain no OAuth token.
7. A simulated 429 does not select or create a usage-billed fallback.
8. Disconnecting MUR leaves `codex login status` authenticated.
9. Confirmed global logout leaves Hub and Codex CLI unauthenticated and marks
   retained registry entries unhealthy.

## Delivery Slices

The work spans coupled components but can land in dependency order:

1. **Runtime contract:** add `provider: codex`, authless loopback enforcement,
   shared OpenAI codec extraction, and registry fixtures.
2. **Hub control-plane backend:** supervised app-server adapter, login/status/
   logout commands, model pagination, gateway status/install/start commands.
3. **Hub product surface:** dedicated provider panel, state machine, model
   selection, billing labels, disconnect/logout semantics, and en/zh-TW copy.
4. **Cross-repo contract hardening:** non-billable gateway readiness signal if
   needed, integration fixtures, and real end-to-end verification.

Each slice must keep existing OpenAI API-key behavior passing. The Hub surface
must not ship as generally available until the runtime and installed gateway
contracts are present, or it would create registry entries that cannot dial.

## Documentation

- Explain that `codex login` uses ChatGPT subscription access while an OpenAI
  Platform API key is usage billed.
- Document the fixed local gateway path and why no key is entered.
- Document disconnect versus global logout.
- Document the 429 policy and fallback billing labels.
- Link to the official OpenAI authentication and Codex app-server model-list
  documentation rather than documenting credential-file internals for users.

## Resolved Questions

- Authentication owner: Codex, surfaced by Hub.
- Provider presentation: independent ChatGPT Subscription provider.
- Model source: app-server `model/list`, manual unverified fallback.
- Gateway lifecycle: guided start/install with consent.
- Runtime identity: dedicated authless, loopback-only `codex` provider.
- Logout: separate from MUR disconnect.
- Billing fallback: never add or select usage-billed fallback implicitly.
- Control/data split: app-server for account/models, gateway for inference.

No product decisions remain open. Exact Rust/TypeScript type names, Tauri
command names, and backward-compatible metadata field placement belong in the
implementation plan.
