# MUR Hub Claude Subscription — Design

**Status:** Approved (brainstorm 2026-09-03)

Sibling of `2026-09-02-mur-hub-chatgpt-subscription-design.md`. Where this
document says "same as ChatGPT", that design and its shipped code
(mur#1154, mur-model-gateway#11) are the reference; only the differences are
spelled out here.

## Problem

`mur-model-gateway` has attached the Claude Code OAuth token to `/v1/messages`
requests since its first release, and the Hub already suggests the gateway
URL when a user adds an Anthropic provider. What is missing is everything
the ChatGPT provider added on top of its data plane:

- **No safety property.** `provider: anthropic` + `base_url: http://127.0.0.1:8088`
  is a subscription entry; change one line to `https://api.anthropic.com` and
  the same entry is usage-billed. The runtime cannot tell the two apart, so
  nothing stops the change.
- **No control plane.** The Hub cannot say whether Claude Code is signed in,
  with which account, or whether that login is a subscription (`claude.ai`)
  or an Anthropic Console (API-billed) login. `/__mur/health` reports the
  Codex credential kind but not the Claude one.
- **No dedicated entry.** The Model Library's Anthropic card is the API-key
  flow with an optional gateway URL — the subscription is a side effect of
  editing a field, not a provider a user chooses.

## Product Decision

A distinct **Claude Subscription** provider, mirroring ChatGPT Subscription
one-for-one: its own wire provider `claude`, no secret, loopback-only, an
account view from `claude auth status`, login/logout wrapped from
`claude auth`, and the same billing labels and paid-fallback rules.

Existing `provider: anthropic` entries — including those already pointed at
the gateway — keep working unchanged. `mur model doctor` gains a warn-only
hint that such an entry would be safer as `provider: claude`.

## Canonical Terms

- **Claude Subscription** — a Claude Pro/Max plan used through Claude Code's
  login. Registry `provider: claude`. Billing `subscription`.
- **Anthropic (API)** — the existing `provider: anthropic` with an API key.
  Billing `usage_billed`. Unchanged.
- **Console login** — `claude auth status` reporting `authMethod: console`:
  Claude Code signed in with an Anthropic Console account (API billing). Not
  a subscription; renders as "signed in, but API-billed — not this provider",
  exactly like Codex `apiKey`.
- **Gateway** — `mur-model-gateway`, the loopback data plane. Same binary
  and service as the ChatGPT provider.

## Goals

- `provider: claude` sends authless Anthropic Messages requests only to
  `http://<localhost|loopback>:<port>/v1`; a secret, a remote host, `https`,
  userinfo, query, or fragment is refused at startup.
- Hub shows signed-in state, email, and login kind from `claude auth status`.
- Hub wraps `claude auth login --claudeai` and a confirmed `claude auth logout`.
- `/__mur/health` reports the Claude credential kind (`oauth` / `missing`).
- Model list from the models.dev catalog; an unverified manual id when the
  catalog is unavailable.
- One panel implementation serves both subscription providers.

## Non-goals

- Changing `provider: anthropic` behaviour or migrating existing entries.
- Reading, parsing, or logging the Claude Code credential (keychain blob or
  `~/.claude/.credentials.json`) from Hub or runtime.
- Refreshing the OAuth token — the gateway already delegates that to
  `claude auth status` on a 401.
- A `/v1/models` probe: the gateway would forward it with the OAuth token,
  and the endpoint is not part of the subscription contract.

## Considered Approaches

### A. Distinct `claude` provider + generalized panel (selected)

Same shape as `codex`. Safety lives in the provider string, existing entries
are untouched, and the ChatGPT panel becomes a parameterized
`SubscriptionProviderPanel`. Cost: `provider` is otherwise "the wire
protocol", and `claude` speaks the same Messages API as `anthropic`. Accepted:
the distinction the runtime must enforce is *who pays and where it may
connect*, and that is a property of the entry, not of the URL.

### B. Runtime + health only, panel later

Lands the safety property without UI. Rejected as the whole scope because
users would still hand-write YAML; kept as the natural first delivery slice.

### C. A "via gateway" toggle on the Anthropic card

No new provider. Rejected: cannot stop a one-line `base_url` edit from
turning a subscription entry into a metered one, which is the property the
ChatGPT work exists to guarantee.

## Architecture

```
MUR Hub ── claude auth status/login/logout ──► Claude Code CLI ──► OS keychain
   │                                                                   ▲
   │ GET /__mur/health (claudeCredential: oauth|missing)               │ reads
   ▼                                                                   │
mur-model-gateway 127.0.0.1:8088 ── /v1/messages + Bearer (OAuth) ──► api.anthropic.com
   ▲
   │ authless Messages, loopback only
Agent runtime (provider: claude)
```

### Component boundaries

#### `SubscriptionProviderPanel` (Hub UI)
`ChatGPTSubscriptionPanel` renamed and parameterized by a descriptor:
`{ key, provider, name, logo, color, commands: { accountRead, modelsList,
login, logout }, i18nPrefix }`. `deriveChatGPTState` becomes
`deriveSubscriptionState`; its states, precedence, and tests are unchanged.
`CHATGPT_SUBSCRIPTION` and a new `CLAUDE_SUBSCRIPTION` descriptor live in
`modelLibraryHelpers.ts`.

#### `claude_subscription` (Hub Tauri)
- `account.rs`: runs `claude auth status`, parses only `loggedIn`,
  `authMethod`, `email` from its JSON; `logged_in` is true only for
  `authMethod == "claude.ai"`. Any other field is dropped. The output is
  bounded and sanitized by the shared `run_bounded`.
- Login: `claude auth login --claudeai`; five-minute timeout, process-global
  mutex, success decided by a following `auth status`, never by exit code.
- Logout: `claude auth logout`, refused without `confirmed: true`.
- Model list: `mur_core::model_prices` catalog cache filtered to the
  `anthropic` vendor; `catalog_verified: true`. Catalog unavailable →
  the panel's unverified-id field.
- Gateway status/install: reuse `chatgpt_subscription::process` as is; the
  install line already sets the Codex source and leaves the Anthropic source
  at its keychain default. Move `process.rs` and `run_bounded` to a shared
  `subscription/` module if the second consumer makes that cleaner.
- Registry: `claude_models_add` / `claude_disconnect`, same rules as the
  codex pair (existing alias wins; disconnect removes only
  `provider == claude && billing == subscription`).

#### `ClaudeClient` (runtime)
`AnthropicClient` gains an `AnthropicAuth { ApiKey(String), None }` the way
`OpenAiClient` gained `OpenAiAuth`; every existing constructor stays
`ApiKey`. `ClaudeClient` wraps the authless variant and validates the base
URL with the codex validator generalized over the required path (`/v1`
here, `/codex/v1` there). Factory arm `"claude"`: reject `secret`, require
`base_url`, never consult `ANTHROPIC_API_KEY` or the agent keychain.

#### Gateway
`/__mur/health` adds `claudeCredential: "oauth" | "missing"`, derived from
`TokenSource::resolve_credential()` on the Anthropic source (keychain or
credentials file); never the token, never the expiry. The Hub treats only
`oauth` as ready.

## Hub State Model

Identical to ChatGPT: loading → codex-missing (here: `claude` CLI missing) →
logged-out (includes Console login, with a distinct message) → account error
→ gateway missing → gateway unusable (`not-running` / `hook-missing` is not
applicable — the Anthropic path needs no compiled hook — so the descriptor
declares which health fields gate readiness: `claudeCredential == oauth`) →
models loading → ready.

## Registry Contract

```yaml
claude_opus_5:
  provider: claude
  model: claude-opus-5
  base_url: http://127.0.0.1:8088/v1
  tier: frontier
  billing: subscription
  catalog_verified: true
```

No `secret`. `mur model doctor` warns (never rewrites) when a
`provider: anthropic` entry's `base_url` is a loopback gateway URL.

## Runtime Contract

`validate_subscription_base_url(raw, required_path)`: `http` scheme, host
`localhost` or a loopback IP, explicit port, no userinfo/query/fragment,
normalized path equal to `required_path`. `codex.rs` calls it with
`/codex/v1`, `claude.rs` with `/v1`.

## Logout and Disconnect Semantics

Same as ChatGPT. *Disconnect MUR* removes registry entries only.
*Sign out of Claude* runs `claude auth logout` after confirmation and the
copy names Claude Code sessions and IDE extensions as affected.

## Error Handling

Same classes as ChatGPT (`CliMissing`, `Spawn`, `Timeout`, output bounded to
32 KiB, control characters stripped). `claude auth status` returning
non-JSON or `loggedIn: false` is logged-out, not an error; a non-zero exit
with JSON is still parsed.

## Fallback and Billing Safety

No change: billing labels, `paidFallbackWarning`, and `pickNextFallback` are
provider-agnostic. A subscription Claude primary with an `anthropic` (API)
fallback warns exactly as a ChatGPT primary with an `openai` fallback does.

## Security and Privacy

- Hub and runtime never open the keychain entry or credentials file.
- `claude auth status` output beyond the three fields is discarded before it
  can reach a view, log, or diagnostic (it carries `orgId` and paths).
- Health carries a kind, never a token or expiry.

## Testing

- Runtime: URL table for `/v1` (accepts loopback forms, rejects
  api.anthropic.com, https, wrong path, userinfo, query); factory rejects
  secret/remote/missing URL; `AnthropicClient::new` still sends `x-api-key`.
- Hub: fake `claude` script answering `auth status` with `claude.ai`,
  `console`, and `loggedIn:false` JSON; login exit 0 + console status →
  not authenticated; logout without confirmation spawns nothing; catalog
  unavailable → models error state. All under `FAKE_BIN_LOCK`.
- Gateway: health with `TokenSource::Disabled` → `missing`; with a
  credentials-file fixture → `oauth`; body contains no `sk-ant`.
- UI: descriptor-driven state tests run once per descriptor.
- Acceptance: sign in from Hub, add a model, one turn on a disposable agent,
  gateway log shows `provider=Anthropic` and the request carried no
  `x-api-key`; editing the entry's `base_url` to `https://api.anthropic.com`
  makes the agent refuse to start; Disconnect leaves `claude auth status`
  signed in.

## Delivery Slices

1. Gateway `claudeCredential` in health.
2. Runtime `AnthropicAuth` + `ClaudeClient` + factory arm + shared validator.
3. Hub Tauri `claude_subscription` (account, login/logout, registry) and the
   `process.rs` relocation.
4. Hub UI panel generalization + `CLAUDE_SUBSCRIPTION` descriptor + copy
   (en, zh-TW).
5. `mur model doctor` hint; docs (`docs/model-gateway.md`, README, docs site,
   product page).

## Resolved Questions

- **Provider identity:** distinct `claude` string (over an `anthropic` flag or
  `anthropic-subscription`).
- **Account source:** `claude auth status` JSON, because health cannot
  distinguish a Console login; health remains the gateway-readiness signal.
- **Login/logout:** wrapped (`claude auth login --claudeai`, `claude auth
  logout`) — verified present in Claude Code 2.1.258.
- **Health credential kinds:** `oauth | missing` only — the keychain blob has
  no auth-method field, so `console` is not derivable there.
