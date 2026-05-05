# mur — Privacy Statement

**Last updated:** 2026-05-05
**Applies to:** mur v2.6.x and later (v1 release line)
**Scope:** the `mur` CLI, the per-agent `mur-agent-runtime` supervisor, the optional `mur-agent-gui` desktop bundle, and the bridges shipped in v1 (Telegram, webhook, A2A).

This document tells you exactly what mur does with your data — what stays on your machine, what leaves it, what we redact, and how to turn things off. It is the canonical reference for the privacy claims in `README.md` and on app.mur.run.

---

## 1. What stays on your device

The following data is written only to the local filesystem under `~/.mur/`. mur does **not** upload, sync, or back up this data to any service:

| Data | Path | Notes |
|---|---|---|
| Agent profiles | `~/.mur/agents/<name>/profile.yaml` | identity, entitlements, model_ref |
| Agent identity keys | `~/.mur/agents/<name>/identity.{key,pub}` | Ed25519 keypair, `0600` mode |
| System prompts | `~/.mur/agents/<name>/sys_prompt.md` | including ejected voice templates |
| Skills | `~/.mur/agents/<name>/skills/` | user-installed skill bundles |
| Patterns | `~/.mur/patterns/*.yaml` | learning + memory layer |
| Workflows | `~/.mur/workflows/*.yaml` | multi-step task definitions |
| Session recordings | `~/.mur/session/recordings/*.jsonl` | append-only event log of recorded sessions |
| Companion state | `~/.mur/agents/<name>/companion/state.yaml` | rhythm picker bandit state, content pool |
| Inbox messages | `~/.mur/agents/<name>/companion/inbox/*.md` | proactive companion sends, with front-matter |
| Telemetry | `~/.mur/agents/<name>/telemetry/<date>.jsonl` | OTel-shaped events, see §3 |
| Bridge state | `~/.mur/agents/<name>/bridge/` | sled DedupeStore for A2A / Telegram / webhook |
| LanceDB index | `~/.mur/lancedb/` | vector index, fully rebuildable from YAML |

The data lives on your local disk under your user account. **Time Machine, iCloud, OneDrive, Dropbox, and equivalent OS-level backup tools may surface this directory.** If you don't want backups capturing it, exclude `~/.mur/` from your backup tool's scope.

## 2. What leaves your device

mur sends data over the network only via these explicit, user-authorized paths:

### 2.1 Model provider

The agent's configured LLM provider receives the prompts you (or your enabled bridges) send to the agent. Provider is per-agent; configure via `mur model add` and `mur agent profile set model_ref`. Default providers in v1 are:

- Anthropic API (`api.anthropic.com`)
- OpenAI API (`api.openai.com`)
- Local Ollama (`http://localhost:11434` — does not leave your device)

The B0 rule-7 outbound credential pre-filter scans every outbound payload before it leaves the runtime; if a known credential pattern is detected (OpenAI / Anthropic / AWS / GitHub / GCP / JWT / PEM / Slack-webhook / `.env`-style assignment), the message is **dropped entirely** with a self-correct reason returned to the agent. This catches accidental key inclusion before any provider sees it.

### 2.2 MCP servers

Any MCP server you explicitly added with `mur agent mcp add` will receive tool calls and their arguments. The set of allowed MCP hosts is gated by `entitlements.network.outbound.allowlist` (B0 rule 2); first-time access to a new host triggers an `AskUser` permission prompt that you can either deny or grant with "remember for this agent".

In v1, MCP server binaries are SHA-256 + description-hash pinned at install time (B0 rule 11); a hash mismatch on subsequent loads refuses to spawn the binary.

### 2.3 Bridges (opt-in only)

Bridges are off by default. Each is a separate enable step, and each routes through the same B0 hook chain so the rules above (allowlist, secret pre-filter, untrusted-input wrapping, side-effect cooldown) apply uniformly:

- **Telegram bridge** (`mur agent telegram enable …`) — Long-poll connection to `api.telegram.org`. **Telegram is not end-to-end encrypted**; messages pass through Telegram's infrastructure. Voice transcription via whisper.cpp runs locally; no audio leaves your device.
- **Webhook receiver** (`mur agent webhook enable …`) — Inbound HTTP listener bound to `127.0.0.1:<port>`. Accepts HMAC-SHA256-signed POSTs only. By default, only loopback; expose to your LAN explicitly via the bind address.
- **A2A peer** (transport.tcp / transport.unix-socket) — Agent-to-agent v0.3 over Noise XK; only peers whose pubkeys you've registered are reachable.

### 2.4 What does NOT leave your device

- **Voice audio.** Whisper.cpp runs entirely locally with the bundled large-v3-turbo q5_1 weights. No raw audio or transcripts are uploaded; the transcript is fed to the same LLM call as any other prompt input (so it inherits the model-provider routing in §2.1).
- **OCR text from dropped images.** Vision.framework on macOS, tesseract elsewhere — both on-device. The OCR'd text is fed to the LLM under an `<untrusted_image_text>` wrapper.
- **PDF text content.** `pdfium-render` extracts locally; `<pdf_text>` wrapper prevents the agent from following embedded directives.
- **Companion subsystem proactive content.** The companion module has no direct outbound HTTP; the only network call is to the agent's configured model provider (same as any tool call). Compile-time enforced by `companion::network_audit` — the build fails the moment a companion file imports `reqwest` / `tokio::net` / etc.
- **Identity keys.** Ed25519 private keys never leave the local filesystem.

## 3. Telemetry — what's written, what's redacted

mur writes per-event telemetry to `~/.mur/agents/<name>/telemetry/<YYYY-MM-DD>.jsonl`. **This file stays on your machine** unless you explicitly point a transport subscriber at it.

### 3.1 Always written (operational metadata)

- LLM call: token counts (input/output), model name, provider name, latency, cost, trace ID
- Tool call: MCP server name, tool name, duration, success boolean
- Heartbeat: uptime, memory MB, active task count
- Task progress: task ID, stage name

We need these to debug agent behavior, surface cost/latency in the dashboard, and detect runtime issues. None of them embed user content.

### 3.2 Redacted before write (rule 9 / M8.1)

Free-form string fields go through a single chokepoint redactor before they hit the disk:

- **Credentials** matching the M7.5 regex set (OpenAI / Anthropic / AWS / GitHub / GCP / JWT / PEM / Slack-webhook / `.env`-style assignment) → replaced with `[REDACTED:<label>]`.
- **Home-directory paths** (`/Users/<u>/`, `/home/<u>/`, `C:\Users\<u>\`) → collapsed to `~/` so OS account names don't leak.

This applies to:
- `Error.message` (anyhow context chains often quote file paths)
- `Warning.message`
- `TaskProgress.message`
- Every string leaf of `HookFired.attrs` (hooks may include arbitrary payload — the redactor recurses)

### 3.3 How to disable telemetry

Set in `~/.mur/agents/<name>/profile.yaml`:

```yaml
telemetry:
  enabled: false
```

The writer thread silently no-ops; no `telemetry/*.jsonl` is created. The B0 hook chain still runs (the security rules are independent of telemetry).

## 4. Crashlogs

If the runtime panics, the supervisor captures a panic backtrace to `~/.mur/agents/<name>/crashlogs/<timestamp>.log`. This file stays local. Backtraces may include source file paths and variable names; we recommend not sharing crashlogs with third parties without redacting them. The redactor in §3.2 does **not** currently apply to crashlog content (deferred to v1.1).

## 5. Bridges and platform disclosures

### 5.1 Telegram

Telegram conversations are **not end-to-end encrypted** (Secret Chats are an exception, but the Bot API doesn't support them). When you enable the Telegram bridge, every message you receive or send through it is visible to Telegram's infrastructure. The bot token is stored in your OS keychain; mur never logs it.

If you need an encrypted bridge, prefer the A2A or webhook transports.

### 5.2 Webhook

The webhook receiver listens on `127.0.0.1:<port>` by default. HMAC-SHA256 signature verification is mandatory; messages without `X-Mur-Signature` are rejected with 401. Per-source token-bucket rate limiting prevents abuse from misconfigured callers.

### 5.3 A2A

Agent-to-agent v0.3 transport uses Noise XK over TCP. Only peers whose Ed25519 pubkeys you've registered (`mur agent peer add`) can complete the handshake. Identity keys rotate via `mur agent rekey` with a 30-day grace window for previous-key acceptance.

## 6. Telemetry collection by us (Anthropic / mur.run)

mur **does not** phone home, send anonymized usage metrics to mur.run, or collect crashlog data unless you explicitly opt in. The dashboard at app.mur.run requires you to log in and explicitly upload data; no agent talks to mur.run by default.

## 7. Children's privacy

mur is a developer tool not intended for children under 13 (or the equivalent minimum age in your jurisdiction). Do not configure mur with credentials belonging to a child.

## 8. Changes to this statement

The current version of this statement lives at `docs/release/privacy-statement.md` in the [mur repository](https://github.com/mur-run/mur). Material changes will be called out in the release notes.

## 9. Reporting a vulnerability

Privacy or security issues: open a private security advisory at https://github.com/mur-run/mur/security/advisories or email security@mur.run. Please don't file a public issue for security reports.

---

## Mapped to the v1 B0 baseline

| Privacy claim | B0 rule | Implementation |
|---|---|---|
| Voice never leaves the device | — | whisper.cpp local; documented v1 assumption |
| OCR / PDF text stays local | rule 14, 16 | Vision.framework / tesseract / pdfium-render |
| Telemetry redacts secrets + home paths | rule 9 | `redact_envelope` chokepoint (M8.1) |
| Companion has no direct egress | rule 12 | `companion::network_audit` (M8.3) |
| New outbound hosts require user consent | rule 2 | network allowlist + AskUser (M7.3) |
| Outbound credentials dropped pre-send | rule 7 | `scan_for_secrets` (M7.5) |
| Memory writes redacted | rule 8 | `redact_pii` in `post_tool_use` (M7.6) |
| MCP binaries pinned | rule 11 | SHA-256 + description hash on install (M7.7) |
| Same-turn cooldown after untrusted input | rule 4, 17 | `pre_tool_use` turn flag |
| Untrusted input wrapped in spotlighting tags | rule 3, 18 | `<untrusted_*>` envelopes (M7.4) |

For the precise spec text, see `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.1.
