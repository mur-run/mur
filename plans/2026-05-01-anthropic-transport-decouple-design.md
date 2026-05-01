# Decouple Anthropic OAuth handling from public mur transport

**Status:** Draft
**Date:** 2026-05-01
**Supersedes (in part):** [`plans/Anthropic-oauth-token.md`](Anthropic-oauth-token.md)

## Background

mur and mur-commander both speak the Anthropic Messages API directly. The current implementation in mur (and the mirrored implementation in mur-commander) does three things at once inside the public client code:

1. **Transport** — POST to `https://api.anthropic.com/v1/messages`
2. **Auth selection** — branch on key prefix (`sk-ant-api03-*` → `x-api-key`; `sk-ant-oat*` → `Authorization: Bearer`)
3. **OAuth-specific shaping** — beta headers, system-prompt billing prefix, macOS Keychain token sourcing (see [`plans/Anthropic-oauth-token.md`](Anthropic-oauth-token.md) for the full set)

Concretely:

| Location | Behavior |
|---|---|
| `mur-core/src/llm.rs:220` | Hardcoded `https://api.anthropic.com/v1/messages` |
| `mur-core/src/llm.rs:94-130` | OAuth `BILLING_HEADER`, beta constant, macOS-only Keychain reader |
| `mur-core/src/llm.rs:189-250` | OAuth-aware Anthropic client |
| `mur-agent-runtime/src/llm/anthropic.rs` | Reads `ANTHROPIC_BASE_URL`, but ALSO embeds OAuth disguise inline |
| `mur-commander gateway/...` (~10 sites) | Hardcoded URL + duplicate OAuth handling |

This conflates concerns. It also hardcodes a version-pinned billing constant (`cc_version=2.1.77`) that drifts every time Claude Code releases a new version.

## Problem

1. **Cross-cutting concern in core code** — every Anthropic call site has to know about OAuth shape. Net effect: duplicated logic across two repos and ~13 sites.
2. **Version drift** — the billing constant is brittle and platform-specific.
3. **Platform fragmentation** — Keychain read is macOS-only; Linux/Windows users with the same Claude Code subscription get a degraded path.
4. **Limited transport flexibility** — hardcoded URLs prevent legitimate use cases: AWS Bedrock, GCP Vertex, corporate egress proxies, integration test fixtures.

## Goals

- Public mur and mur-commander become **provider-neutral Anthropic clients**: configure base URL, send key, marshal request/response. No OAuth-specific code paths.
- Auth specifics (token sources, refresh, custom headers, content prefixes) live in **an external service** the user points at via `ANTHROPIC_BASE_URL`.
- Existing OAuth users have a clear migration path.

## Non-goals

- The external auth service is **out of scope for this repo**. Users (or downstream tooling) provide their own. This doc only specifies the contract mur expects.
- No change to non-Anthropic providers (OpenAI, Gemini, Ollama, OpenRouter).

## Design

### Phase 1 — `ANTHROPIC_BASE_URL` everywhere (pure refactor)

Add a single helper:

```rust
// mur-common/src/lib.rs (or mur-core/src/llm.rs)
pub fn anthropic_base_url() -> String {
    std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_string())
}
```

Replace every hardcoded `https://api.anthropic.com/v1/messages` with `format!("{}/v1/messages", anthropic_base_url())`:

- `mur-core/src/llm.rs:220`
- `mur-commander/crates/engine/src/model/provider.rs:100`
- `mur-commander/crates/gateway/src/unified_handler/llm_service/mod.rs` (lines 325, 474, 912, 914, 1677, 1679)
- `mur-commander/crates/gateway/src/unified_handler/llm_service/agentic.rs` (lines 431, 1291)
- `mur-commander/crates/gateway/src/unified_handler/llm_service/call.rs` (lines 283, 416)
- `mur-commander/crates/gateway/src/unified_handler/browse_handler/mod.rs:123`

Sites that already pull base URL from config (`call.rs:142`, `mod.rs:172`) keep doing so but consolidate behind the same helper.

This phase is **independently mergeable** to upstream. Justification: support Bedrock-compatible endpoints, Vertex, corporate egress proxies, test harnesses. Net positive for all users regardless of OAuth.

### Phase 2 — Remove OAuth-specific branches from public clients

After Phase 1, public clients delete:

- `is_anthropic_oauth_token` and all branches gated on it
- `BILLING_HEADER` constant
- `ANTHROPIC_OAUTH_BETAS` / `OAUTH_BETAS` constants
- `read_oauth_from_keychain` (macOS-specific)
- The conditional system-prompt prefix injection
- The conditional auth-header switch (always send `x-api-key` with whatever's in the key env var)

What remains in the public client:

- Marshal `LlmMessage{role:"system"}` → top-level `system` field (Anthropic schema requirement, not OAuth-specific)
- Honor `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL`
- Standard `anthropic-version` and `content-type` headers

### Phase 3 — External auth contract (informative)

Users requiring OAuth-style flows run a local Anthropic-compatible HTTP service. The service:

- Accepts the same Messages API surface mur sends
- Is responsible for any auth transformation (token sourcing, header injection, content prefixing, refresh)
- Streams responses back unchanged

Users configure mur via:

```sh
export ANTHROPIC_BASE_URL="http://127.0.0.1:8088"
```

For agent runtime daemons (which don't inherit shell env), set `ANTHROPIC_BASE_URL` in the launchd / systemd unit.

The implementation, packaging, and distribution of such a service is **explicitly out of scope** for this repo and is left to downstream tooling. Reference behavior the service is expected to implement is documented in [`plans/Anthropic-oauth-token.md`](Anthropic-oauth-token.md).

## Cross-platform considerations

The current public OAuth path is macOS-only (`security` shell-out). Removing it from mur **improves** Linux and Windows parity — those users now get the same provider-neutral client. Any platform-specific token handling (libsecret on Linux, Credential Manager on Windows) is the external service's concern, abstractable via crates like [`keyring`](https://crates.io/crates/keyring) and [`directories`](https://crates.io/crates/directories).

## Migration

| Step | Action | Breaking? |
|---|---|---|
| 1 | Phase 1 lands in mur and mur-commander | No |
| 2 | Release notes announce Phase 2 + recommend external service for OAuth users | No |
| 3 | Phase 2 lands | **Yes for OAuth users** — they must run an external service or switch to a regular API key |

OAuth users between Phase 1 and Phase 2 can already start migrating: stand up the service, set `ANTHROPIC_BASE_URL`, verify, then upgrade.

## Risks

1. **OAuth users who don't read release notes** — silent regression to 401/429 on Phase 2.
   - *Mitigation:* loud `WARN` on first request if a `sk-ant-oat*` key is detected and `ANTHROPIC_BASE_URL` is unset, suggesting migration.
2. **mur-agent-runtime daemons miss env var** — daemons spawned by launchd/systemd don't inherit shell env.
   - *Mitigation:* document required `EnvironmentVariables` / `Environment=` entry in agent install templates.
3. **Test suites assume direct upstream** — anywhere we mock or assert on `https://api.anthropic.com`.
   - *Mitigation:* grep tests for the literal URL during Phase 1; route through the helper.

## Open questions

- Should the helper also normalize trailing slashes? (Yes — strip trailing `/` before joining `/v1/messages`.)
- Should `ANTHROPIC_BASE_URL` be read once at startup or per-request? (Per-request — cheap and supports test isolation.)
- Should we support a per-agent / per-model base URL override in `models.yaml`? (Probably yes — already partially present; consolidate in a follow-up.)

## Out of scope

- Implementation of any external auth service
- OAuth refresh flow
- Token storage abstractions beyond what already exists in `mur agent secret`
- Provider clients other than Anthropic
