# Feather — Install MCP servers by URL from the MUR Hub

**Status:** Design / spec
**Date:** 2026-06-27
**Codename:** feather
**Scope:** Add a first-class "add an MCP server by URL" flow to the MUR Hub GUI, covering the three MCP distribution models, safest-first.

---

## 1. Problem

Today the Hub can only attach an MCP server to an agent two ways:

- **Local command** — a form in `DetailPanel.tsx` (id + command + args), wired to the `agent_mcp_add` Tauri command.
- **Discover** — `McpDiscoverModal.tsx` scans *other installed tools* (Claude Desktop/Code, Cursor, Gemini CLI, Codex…) and imports one via the same `agent_mcp_add`.

There is **no way in the Hub to add a server by URL**, even though the MUR CLI already supports the two remote paths (`mur agent mcp add-remote`, `mur agent mcp registry-add`, `mur agent mcp login`). And there is **no supported way at all** — CLI or GUI — to download-and-install a standalone MCP server binary from a URL.

Users increasingly receive MCP servers as: a hosted URL (e.g. `https://mcp.notion.com/mcp`), a package name (`@scope/server` / a PyPI/registry name), or — rarely — a direct artifact link. Feather makes all three first-class in the Hub, while making the safe paths easy and the dangerous one explicit and gated.

## 2. Background research (2026 best practice)

Summarized from a deep-research pass (MCP spec `2025-11-25` + `draft/basic/authorization`, RFC 9728, Cloudflare Agents docs, Invariant Labs, CyberArk, OWASP MCP Cheat Sheet, Anthropic custom-connectors docs). Transport claims were adversarially 3-vote confirmed; auth/security claims are direct quotes from the named primary sources (a session limit cut the independent cross-check short — treat as authoritative-source, not independently re-verified).

**Transports.** Streamable HTTP (spec `2025-03-26`) is the current remote transport: a **single endpoint** (conventionally `/mcp`) handling POST (JSON-RPC) + optional GET (SSE stream). It **replaces** HTTP+SSE, which is deprecated (commonly cited sunset **30 June 2026**). Backward-compatible client detection: POST `InitializeRequest`; on 400/404/405 fall back to a GET SSE stream expecting an `endpoint` event. Require HTTPS; validate `Origin`.

**Auth.** OAuth 2.1 + PKCE (S256) is the sanctioned flow. Discovery via RFC 9728 Protected Resource Metadata: a `401` carries `WWW-Authenticate: …resource_metadata="…/.well-known/oauth-protected-resource"`; the client fetches it, reads `authorization_servers`, and runs the flow (optionally with Dynamic Client Registration, RFC 7591). RFC 8707 Resource Indicators bind the token's audience to the specific server (defeats token passthrough / confused-deputy). Static bearer tokens are a valid simpler path. **Store tokens in the OS keychain, never plaintext.**

**Security (the real threat is the tool-definition surface).** Tool Poisoning (hidden instructions in tool *descriptions*), Full-Schema Poisoning (any schema field), Advanced Tool Poisoning (malicious *outputs*/errors), rug-pull (description changes after approval), and cross-server shadowing (a malicious server hijacks calls to a trusted one). Converged defenses: show the **full** tool descriptions at consent (not a simplified label); **pin + hash** tool schemas and re-consent on drift; treat tool outputs as untrusted; **per-server egress allowlist (default-deny)**; least privilege + HITL on privileged tools; verify server identity (TLS).

**Supply chain.** Downloading and running an arbitrary binary from a URL is the `curl | sh` of agents — the highest-risk distribution. Best practice: prefer remote (no local code) → prefer package managers (`npx`/`uvx`, ecosystem-vetted) → treat raw artifact download as a last resort, only with mandatory verification + scan + sandbox.

## 3. Distribution models & ranking

| Model | URL is… | Runs locally? | Trust unit | Risk |
|---|---|---|---|---|
| **Remote** | a hosted MCP endpoint | No (vendor infra) | OAuth scope / token | Low |
| **Package** | npm/PyPI/registry name → `npx`/`uvx` | Managed by pkg mgr | npm/PyPI + registry | Medium |
| **Raw artifact** | a direct binary / `.tar.gz` | Yes, MUR-managed in `~/.mur/mcp-servers/` | the URL alone | High |

Feather builds them **safest-first** (§5).

## 4. What MUR already has (reuse map)

| Capability | Existing primitive | Location |
|---|---|---|
| Add remote (URL + bearer) | `cmd_mcp_add_remote(agent, name, url, bearer: Option<SecretRef>)` | `mur-core/src/cmd/agent/mcp.rs` |
| OAuth 2.1 + PKCE login | `mcp_login` | `mur-core/src/cmd/agent/mcp_login.rs` |
| Registry search + add (`npx`/`uvx`/remote) | `cmd_mcp_registry_add`, registry search | `mur-core/src/cmd/agent/mcp_registry.rs` |
| Per-server egress allowlist | `McpServerNetwork { mode: Off/Restricted, allow_hosts }`, `set-network` | `mur-core/src/cmd/agent/mcp.rs` |
| Binary hash pin / resolve | `agent_mcp_pin::{resolve_command, compute_binary_sha256, build_pinned_entry, cmd_mcp_pin}` (B0 rule 6) | `mur-core/src/cmd/agent/agent_mcp_pin.rs` |
| Secret storage (keychain) | `SecretRef`, `mur agent secret` | `mur-common/src/secret.rs` |
| Profile entry | `McpServerEntry { name, command, args, url, auth: McpAuth::Bearer{token}, network, binary_sha256, … }` | `mur-common/src/agent` |
| Sandboxed MCP child | B1 kernel sandbox wraps spawned MCP children | `mur-agent-runtime` |
| HITL on tools | risk-tiered SHA-256 gate | `mur-common/src/hitl.rs`, `mur-core/src/hitl/` |
| Self-heal install into `~/.mur/mcp-servers` (atomic) | `exec::ensure_bundled_mcp_server` / `install_if_stale` | `mur-common/src/exec.rs` |
| Hub MCP Tauri commands | `agent_mcp_add`, `agent_mcp_remove`, `agent_mcp_toggle`, `mcp_discover` | `mur-hub-gui/src-tauri/src/mcp_skills.rs` |
| Hub MCP UI | `McpDiscoverModal.tsx`, `DetailPanel.tsx` | `mur-hub-gui/ui/src/components` |

**Conclusion:** P1 and P2 are largely *surfacing existing mur-core in the Hub*. P3 is the only path that needs substantial new backend, and it is security-critical.

## 5. Design — phased, safest-first

### Phase 1 — Remote MCP by URL (primary)

**Entry point.** A new "Add by URL" choice next to the existing local-command add and Discover, opening an `McpAddRemoteModal`.

**Form.** URL (required) · Auth: `None | Bearer | OAuth` · Bearer token (when Bearer; stored as a `SecretRef`, never echoed back).

**Flow.**
1. **Validate URL** — must be `https://` (reject `http://` except an explicit `localhost`/`127.0.0.1` dev escape hatch). Normalize.
2. **Connection test (before save)** — POST an `InitializeRequest`. Outcomes:
   - 2xx + valid MCP handshake → Streamable HTTP detected; continue.
   - `401` with `WWW-Authenticate: resource_metadata=…` → server needs OAuth → offer the **OAuth login** flow (reuse `mcp_login`: RFC 9728 discovery → OAuth 2.1 + PKCE → token to keychain).
   - 400/404/405 → attempt legacy SSE detection (GET, expect `endpoint` event); if found, warn it's a **deprecated** transport (sunset ~2026-06-30) but allow.
   - Network/TLS error → surface clearly, do not save.
3. **Consent screen** — after a successful handshake, call `tools/list` and show **every tool with its FULL description** (the anti-tool-poisoning measure), not a simplified label. User confirms.
4. **Save** — `cmd_mcp_add_remote` writes the `McpServerEntry { url, auth }`; bearer/OAuth tokens live in the keychain via `SecretRef`. **Pin a hash of the tool schemas** in the entry (new field; see §6).
5. **Egress** — default the new server's `network` to **Restricted with the URL's host allowlisted** (so its declared endpoint works but nothing else), surfaced as an editable host list.

**New Tauri commands:** `agent_mcp_add_remote(name, server_id, url, auth)`, `agent_mcp_test_connection(url) -> {transport, tools[], needsAuth}`, `agent_mcp_oauth_login(name, server_id, url)`. Each wraps existing mur-core logic.

### Phase 2 — Registry / package install

Surface `cmd_mcp_registry_add` + registry search in the Hub: search `registry.modelcontextprotocol.io`, show publisher, install. Resolves to `npx`/`uvx` (stdio) or a remote URL. **No writes to `~/.mur/mcp-servers`.** Reuses the P1 consent screen + egress default. Mostly a new `McpRegistryModal` + a Tauri wrapper.

### Phase 3 — Verified raw download into `~/.mur/mcp-servers` (gated last resort)

The path the user specifically wants — built last, because it is the only one that runs unvetted third-party code locally. Pipeline:

```
Install MCP from URL  →  ~/.mur/mcp-servers/<name>/
 1. Provenance   prefer a Registry entry (publisher known) over a bare URL; display publisher + URL.
 2. Verify       REQUIRE a sha-256 (or minisign signature). No hash ⇒ blocked unless the user
                 explicitly accepts "unverified" (fail-closed default OFF). Reuse the minisign
                 path already used for .muragent + the Tauri updater.
 3. Download     performed by MUR's TRUSTED control plane (Hub/CLI), NOT inside a sandboxed
                 agent — privilege separation. Egress for the fetch is the control plane's, not
                 the agent's.
 4. Scan         run the existing muragent executable-ban + skill security scanner on the artifact
                 (reject script/interpreter/shared-lib shapes per current rules).
 5. Install      atomic temp+rename into ~/.mur/mcp-servers/<name>/ (same pattern as
                 exec::install_if_stale), chmod 0755.
 6. Run          B1 kernel sandbox + egress DEFAULT-DENY (McpNetMode::Off, or Restricted with an
                 explicit allowlist) + narrow fs + HITL ON for all its tools initially.
 7. Pin          hash the binary (B0 rule 6) AND the tool schemas; re-consent on drift
                 (defeats rug-pull + tool-poisoning). Consent shows full tool descriptions.
```

The controls that make P3 best-practice rather than `curl | sh` are **2 (mandatory verification), 6 (default-deny egress + HITL), and 7 (binary + schema pinning)**. A new `mur agent mcp install-url` CLI command + `agent_mcp_install_url` Tauri command back it; the download/verify/install engine lives in mur-core (CLI + Hub share it). `~/.mur/mcp-servers/<name>/` is the install home, alongside MUR's own bundled `mur-mcp-server`.

## 6. Data model

Add to `McpServerEntry` (mur-common):

- `tool_schema_sha256: Option<String>` — hash over the canonical JSON of the server's `tools/list` (names + descriptions + input schemas), pinned at consent. The runtime re-computes on connect; on mismatch it **pauses the server and requires re-consent** (rug-pull / tool-poisoning defense). Mirrors the existing `description_hash` idea for stdio.
- (P3) `source_url: Option<String>` and reuse existing `binary_sha256` for the downloaded artifact.

`McpAuth` already covers `Bearer { token: SecretRef }`; add an `OAuth { … }` variant if not present (token refs in keychain).

## 7. Security model (cross-phase)

- HTTPS required (localhost dev exception); validate `Origin`/TLS.
- Tokens only in the OS keychain via `SecretRef`; never written to `profile.yaml` or logs.
- Consent always shows **full** tool descriptions; schemas are hash-pinned; drift ⇒ re-consent.
- Every newly added remote/downloaded server gets **egress default to Restricted/Deny** + HITL-on, opt-out explicit.
- P3 downloads: mandatory verification (fail-closed), security scan, sandboxed execution, control-plane-only fetch.
- Aligns with MUR's standing autonomy-safety rule: opt-in, fail-closed, sandboxed, verified.

## 8. Error handling

- Connection test failures (DNS/TLS/timeout/non-MCP response) are shown inline; nothing is saved.
- Duplicate `server_id` → reject (existing `cmd_mcp_add_remote` already bails).
- OAuth cancel/àdenied → no entry written.
- P3: hash mismatch, scan failure, or download error → abort, leave `~/.mur/mcp-servers` untouched (atomic temp dir discarded).
- Profile changes apply on agent **restart** (existing model; the Hub already messages this).

## 9. Testing

- mur-core unit tests: URL validation/normalization; transport detection (200 vs 401-with-metadata vs 400/404/405 fallback); tool-schema hashing + drift detection; P3 verify/scan/atomic-install (reuse `install_if_stale` test pattern); egress-default derivation.
- Hub: Tauri command wrappers return typed results; modal state machine (test → auth → consent → save) with mocked invoke.
- Manual/live: add a real hosted MCP (bearer + OAuth), confirm tools appear and HITL fires; (P3) install a signed artifact and confirm sandbox/egress-deny.

## 10. Non-goals

- Building/compiling MCP servers from source (git clone + build).
- Auto-updating installed P3 servers (manual re-install for now).
- Server-side MCP hosting.
- Changing the existing local-command add or Discover flows beyond sharing the new consent screen.

## 11. Open questions

- P1 default egress: host-only allowlist vs full-deny-then-prompt? (Lean host-only so the server's own endpoint works out of the box.)
- P3: support `npx`/`uvx` package URLs as an install source (collapsing into P2), or strictly raw artifacts?
- Should the consent screen also diff tool descriptions against a known-good corpus / flag suspicious patterns (static scan), or just display them? (P1 displays; static scan could be a later add.)
