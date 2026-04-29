# mur-agent-runtime — Hooks (A0)

> **Frozen contract.** A0 (M0) ships the 10-method `Hook` trait surface.
> Any change to method names, signatures, dispatch semantics, or
> built-in handler registration order is a breaking change and must
> bump `mur_agent_runtime::hooks::HOOK_SCHEMA_VERSION` (currently 1).
>
> User-facing extensibility (config-driven handler picker, plugins,
> WASM, scripts, visual editor) is **not** part of A0. See roadmap §3.3
> (A1-A4 v2 boundary).

## The 10 hooks

| # | Method | Phase | Dispatch | Returns |
|---|---|---|---|---|
| 1 | `on_startup` | Observe | parallel `join_all` | `()` |
| 2 | `on_trigger_fired` | Observe | parallel `join_all` | `()` |
| 3 | `on_message_received` | Observe | parallel `join_all` | `()` |
| 4 | `on_prompt_submit` | **Mutate** | serial fold | `PromptPatch` |
| 5 | `pre_tool_use` | **Gate** | serial short-circuit | `Decision` |
| 6 | `post_tool_use` | Observe | parallel `join_all` | `()` |
| 7 | `on_step_finish` | Observe | parallel `join_all` | `()` |
| 8 | `on_message_send` | **Mutate** | serial fold | `MessagePatch` |
| 9 | `on_error` | Observe | parallel `join_all` | `()` |
| 10 | `on_shutdown` | Observe | parallel `join_all` | `()` |

## Dispatch semantics

- **Gate**: hooks run in chain order. The first non-`Decision::Allow`
  short-circuits; later hooks do not run. Returned `Decision` is what
  the caller sees.
- **Mutate**: hooks run in chain order. Each returns a patch value;
  `HookChain` folds them deterministically (`a.merge(b)` with `b`
  applied after `a`). A panic in handler #N drops only that handler's
  patch; prior patches are committed and the underlying view is never
  observed half-mutated.
- **Observe**: hooks run in parallel via `futures::future::join_all`.
  Errors are logged via `tracing::warn!` and never propagated.

All methods receive `&CancellationToken`; honoring cancellation is the
hook author's responsibility. The dispatcher additionally checks the
token between hooks for gate / mutate phases.

## Built-in handlers (M0)

Registered in this order by `Supervisor::entrypoint`:

1. **`TelemetryHook`** — emits OTel-GenAI 2026 events for every fired
   hook. Sends `Event::HookFired { method, attrs }` through the
   `TelemetryWriter` channel; the JSONL writer flattens `attrs` into
   the notification params alongside the standard agent envelope.
   Sensitive payloads (`gen_ai.input.messages`, `output.messages`,
   `tool.call.{arguments,result}`) are intentionally **not** captured;
   capture is opt-in via
   `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true` and
   mur's redaction pipeline (B0 / M8).

2. **`CompanionVoiceHook`** — adapts the companion phase 1.1 voice
   composition (`companion::voice::compose_*`) into the hook surface.
   `on_prompt_submit` returns the rendered voice as a
   `PromptPatch::set_system_prefix`. **M0 does not register this hook
   in the supervisor by default**; the companion phase 1.1 reactive
   path remains unchanged. Registration plumbing lands when the
   companion subsystem hot-reloads voice on profile change (out of
   M0 scope).

3. **`B0SafetyHook`** — slot reserved; the 22 baseline rules from
   roadmap §6.1 (12 text + 10 multimodal) land in B0's own milestone
   (M8). M0 ships a no-op stub so the chain registration order is
   stable. When implemented, this hook will own:
   - `on_prompt_submit`: untrusted wrappers, secret pre-filter
   - `pre_tool_use`: no-chain-after-untrusted, AskUser triggers,
     `GrantStore` lookup
   - `post_tool_use`: redaction
   - `on_message_received`: untrusted-flag from envelope metadata
   - `on_startup`: sandbox attestation

4. **`LedgerHook`** — slot reserved. Wiring `on_message_send` to
   companion's durable ledger would conflate proactive companion
   sends with reactive replies and corrupt the frozen
   `OutboxEvent::MessageSent { id, channel, sent_at }` schema (R12
   invariant from companion phase 1.1). Real wiring lands when
   reactive-reply ledger semantics are designed.

## `Decision::AskUser` UX (locked)

When `pre_tool_use` returns
`Decision::AskUser { prompt, default, scope_key }`:

- LLM stream pauses; no token burn.
- GUI displays an inline approval card (not modal) with four buttons:
  `Allow once` / `Allow for this agent (30d)` / `Deny once` /
  `Deny + remember`. **There is no "all agents" scope, ever.**
- When the window is unfocused, also surfaces a Tauri tray badge +
  OS notification.
- Trust anchor in the UI is the tool name + a structured input table.
  The LLM-authored rationale renders in a muted secondary block
  labeled "Agent says: (untrusted)", capped 500 chars, ANSI / markdown
  control chars stripped.
- 120 s timeout → auto-Deny.
- Headless (no GUI attached) → auto-Deny + audit event
  `headless_denied`. Never queue.
- "Allow for this agent" persists 30 days renewable in
  `~/.mur/agents/<name>/permissions/grants.yaml`.
- Every decision appends to `permissions/audit.jsonl` (append-only,
  never mutated).
- Revocation lives in Tauri Settings → Permissions tab (mirrors
  macOS TCC's principle that revocation is outside the app being
  governed).

`ScopeKey` = `(agent_id, tool_name, sha256(canonical_input_subset))`.
Each tool declares which input fields contribute to the SHA-256
(e.g., `bash` hashes `argv[0]` only; `fs.write` hashes the directory
prefix) — avoiding the "rm -rf foo whitelists rm -rf *" overreach
footgun documented in Cursor 0.43+.

## OTel-GenAI 2026 attribute migration

A0 (M0.2.1) migrated `mur-common::telemetry`:

**Removed:** `gen_ai.system` (deprecated by spec in 2025).

**Added (gen_ai.* core):**
`gen_ai.provider.name`, `gen_ai.operation.name`,
`gen_ai.response.{model,finish_reasons}`.

**Added (gen_ai.agent.* + correlation):**
`gen_ai.agent.{id,name}`, `gen_ai.conversation.id`.

**Added (gen_ai.tool.*):**
`gen_ai.tool.{name,type,call.id}`.

**Added (Stable spec namespaces):**
`error.type`, `mcp.method.name`, `mcp.session.id`,
`network.transport`.

**Added (mur.* extensions, no spec coverage):**
`mur.cost_usd`, `mur.trigger.kind`, `mur.a2a.peer.pubkey`,
`mur.hook.name`, `mur.hook.phase`. New JSON-RPC notification method:
`telemetry/hook_fired` (`METHOD_HOOK_FIRED`).

## Production call sites — what fires today

| Hook | Production caller (M0) | Production caller (future) |
|---|---|---|
| `on_startup` | ✅ `Supervisor::entrypoint` (after writer spawns, before transports) | — |
| `on_shutdown` | ✅ `Supervisor::entrypoint` (graceful shutdown, before writer drains) | — |
| `on_message_received` | — | A2A protocol method handlers (`message/send`, `tasks/send`) when MCP tool-call loop lands |
| `on_trigger_fired` | — | companion outbox tick + cron + webhook (Track C v1/v2) |
| `on_prompt_submit` | — | TaskRunner LLM call path (current `run_sync` is echo-stub for non-LLM backends) |
| `pre_tool_use` | — | TaskRunner MCP tool-call loop (lights up with Track A/D MCP integration) |
| `post_tool_use` | — | same |
| `on_step_finish` | — | TaskRunner per-step boundary |
| `on_message_send` | — | TaskRunner outbound + companion outbox dispatch |
| `on_error` | — | TaskRunner / supervisor error paths |

The hook chain itself is exercised end-to-end via:

- `mur-agent-runtime/tests/hooks_smoke.rs` — gate / mutate / observe
  semantics, error-drops-patch.
- `mur-agent-runtime/tests/hooks_snapshot.rs` — full Telegram-inbound
  → outbound fire-sequence snapshot (28 events × 4 handlers).

## A1+ (deferred)

The Hook trait is the long-lived contract. v2 layers add extensibility
on top without changing this surface:

- **A1**: config-driven handler picker — `profile.yaml.hooks:` block
  lists from a curated set per hook. No user-defined Rust code.
- **A2**: user-extensible mechanism. Concrete choice (Rust crate
  plugin / WASM via wasmtime / Lua via mlua / Rhai / subprocess over
  Unix socket) is **A2's own design spec** — the A0 trait is
  mechanism-neutral.
- **A3**: composition policy — conditions, retry, parallel vs
  sequential mutate hooks, short-circuit rules.
- **A4**: visual / declarative editor in dashboard / GUI.
