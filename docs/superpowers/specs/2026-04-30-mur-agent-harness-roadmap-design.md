# mur Agent Harness — Roadmap Design

**Status:** Draft (brainstorming output, awaiting per-track plan generation).
**Date:** 2026-04-30.
**Authors:** David + Claude (Opus 4.7, 1M context).
**Predecessors:**
- [`2026-04-22-murmur-p0-agent-runtime-design.md`](./2026-04-22-murmur-p0-agent-runtime-design.md) — P0a per-agent runtime (shipped).
- [`2026-04-23-murmur-fleet-architecture-design.md`](./2026-04-23-murmur-fleet-architecture-design.md) — P0a.5 → P1 → P2 fleet architecture.
- [`2026-04-24-murmur-agent-rekey-design.md`](./2026-04-24-murmur-agent-rekey-design.md) — P0a.6 identity rotation (shipped).
- [`2026-04-29-mur-agent-gui-export-design.md`](./2026-04-29-mur-agent-gui-export-design.md) — `mur agent export --format gui` (shipped via PR #41/#42).
- [`2026-04-29-mur-companion-phase-1-1-design.md`](./2026-04-29-mur-companion-phase-1-1-design.md) — Companion subsystem Phase 1.1 (current branch `feat/companion-phase-1-1`).
**Successors (each its own design spec, written after this roadmap is approved):**
- `2026-04-30-mur-agent-hooks-design.md` (Track A — A0 contract).
- `2026-04-30-mur-delight-pack-design.md` (Track D).
- `2026-04-30-mur-triggers-design.md` (Track C v1).
- `2026-04-30-mur-threat-model.md` (Track B v1 — threat model document).
- `2026-04-30-mur-b0-baseline-design.md` (Track B0 — 22-rule baseline mapped to A0 hooks).

---

## 0. Executive Summary

Now that `mur agent export --format gui` ships click-to-launch agent .app bundles and Companion Phase 1.1 ships a relationship-keyed warm voice with proactive outbox, the next leg is a **harness** that turns an exported agent into something a non-technical consumer would actually love and trust day-to-day. This roadmap defines four parallel work tracks plus one cross-cutting baseline, splits each into a v1 (consumer-first) phase and a v2 (developer-power) phase, locks the contracts between them, and points at the per-track specs that will follow.

The four tracks:

- **Track A — Lifecycle Hooks.** v1 ships **A0**: a frozen 10-method `Hook` trait surface, phase-aware dispatch (gate / mutate / observe), `PromptPatch` fold model, four built-in handlers, OTel-GenAI 2026 telemetry. No user-facing extensibility. v2 (A1-A4) layers config-driven handler picking, user-extensible mechanisms (Rust crate / WASM / scripted), composition policies, and a visual editor.
- **Track D — Consumer Delight Pack.** v1 ships **D1-D5**: local-only voice (Kokoro 82M + whisper.cpp large-v3-turbo q5_1), a 5-step first-memory onboarding wizard, drag-drop with B0 multimodal sanitization, character card import/export (CCv3-based with `extensions.mur` namespace + Ed25519 signing), and a Tauri-channel companion → GUI IPC bridge. v2 adds a shell launcher (D6) only if signal supports.
- **Track C — Triggers.** v1 ships **C1-C3**: an A2A bridge agent pattern (zero-LLM "dumb plumbing" with explicit `routes.yaml`, dedupe, heartbeat via `running.lock`, Ed25519-signed envelopes), a Telegram reference bridge (teloxide 0.13 long-polling, local whisper-rs voice transcription, privacy mode ON, mandatory non-E2E disclosure), and a "send from any app" stack of four lightweight channels (URL scheme deep link + global hotkey + macOS Services menu + drag-to-dock). v2 adds cron, webhook receiver, idle/heartbeat triggers, more platform bridges, and the `.appex` Share Extension.
- **Track B — Security.** v1 ships **B0** (a 22-rule consumer-safe baseline implemented inside A0's `B0SafetyHook`) and a v1 **threat model document** (16 sections, OWASP LLM Top 10 2025 × MITRE ATLAS v4.7 × NIST AI 600-1). v2 ships **B1** (real OS-level entitlement enforcement: `birdcage` 0.9 + Landlock ABI v4 + macOS SBPL + reqwest resolver guard, hooks-first kernel-second). v2.1 ships **B2** (Promptfoo + cargo-fuzz + AgentDojo-50 + HarmBench-50 + InjecAgent-200 + Llama-Guard-3-8B local judge).

A0 (1 week) is the only blocking node; D / C / B0 then run in parallel. v1 estimate: 11-14 weeks single-person, ~8 weeks two-person.

This roadmap deliberately **does not** decide A1+ extensibility mechanism, voice cloning ethics, `.appex` build pipeline, cron / webhook triggers, memory poisoning defense, output-arg sanitizer, Windows full sandbox, or multi-agent collusion red-teaming. Each is named in §7.5 and routed to its own future spec.

---

## 1. Context & Anchors

### 1.1 What's Already Shipped (v1 doesn't redo)

| Capability | Source | Reusable for |
|---|---|---|
| Per-agent runtime (BusyBox-style symlink → `mur-agent-runtime`) | P0a | Track C bridge agents are ordinary P0a agents |
| Ed25519 identity, Noise XK TCP, A2A v0.3 stdio/unix/tcp | P0a / P0a.5 | Track C envelope signing & transport |
| Identity rotation with 30d grace + emergency path | P0a.6 | Long-term hygiene; bridges rotate too |
| `mur agent doctor`, `--format gui` 13-phase pipeline, 5 themes, WCAG AA gate | GUI export | Track D extends this shell |
| Companion Phase 1.1: voice composition, locale heuristic, picker w/ cooldown, schedule, earned_permission, inbox, outbox 12-step ledger, Notifier trait, 8 integration tests | Companion | Track D and A reuse all of this |
| OpenTelemetry-GenAI JSONL telemetry framework | P0a / commander integration | Track A's `TelemetryHook` extends |
| Drafts CLI quarantine pattern (`mur drafts list/show/accept/reject`) | PR #25 | Track D character-card import quarantine |
| `paths::mur_root` + Windows CI hardening Phase 1 | PR #26 | All v1 work uses this |

### 1.2 Five User-Stated Themes Mapped to Tracks

| User theme | Track |
|---|---|
| Active mode (cron / heartbeat) vs passive mode (webhook / subscribe / notify / post) | C (v1: chat-platform inbound + share-from-any-app; v2: cron / webhook / heartbeat) |
| App lifecycle with prompts / skills per stage | A (v1: A0 frozen surface; v2: A1+ user-extensible) |
| Security as top concern | B (v1: B0 baseline + threat model; v2: B1 enforcement; v2.1: B2 red-team) |
| UI/UX delight (pet/colleague avatars, drag-drop, voice, chat from Claude/ChatGPT/Slack) | D (v1: voice + onboarding + drag-drop + cards + IPC; v2: shell launcher) plus C3 (share-from-any-app) |
| Handoff / teamwork / parallel local + remote | A0 contract makes this possible; full implementation is a v2+ track once A1 lands |

### 1.3 Decision Log (Brainstorming → Roadmap)

The following decisions were made interactively during the brainstorming session that produced this roadmap. They are load-bearing and any change requires reopening the brainstorm.

| # | Decision | Captured in |
|---|---|---|
| 1 | Track order: D → C → A → B (with A0 spike pulled forward) | §2 |
| 2 | v1 = consumer-first; v2 = developer-power. Two-phase rollout. | §2 |
| 3 | Wakeup-resume from rate limits is execution discipline (Claude's workflow), not product feature. Not in spec; goes in plan execution preamble only. | §7 |
| 4 | Chat-inbound = A2A bridge agent pattern (Plan A2A peer + MCP outbound passthrough), NOT MCP-server-as-inbound (which the MCP spec does not support cleanly). Bridge has zero LLM ("dumb plumbing"). | §5.1 |
| 5 | One agent = one .app (current model). Shell launcher (D6) deferred to v2 pending signal. | §4.7 |
| 6 | B0 22-rule baseline ships in v1.0 (not split v1.0 / v1.1). | §6.1 |
| 7 | TTS = Kokoro 82M int8 ONNX (not Piper). STT = whisper.cpp large-v3-turbo q5_1 (not small/medium). | §4.1 |
| 8 | PTT hotkey = `Cmd+Shift+'` (apostrophe), user-rebindable. NOT Fn (broken on Touch ID Macs). | §4.1 |
| 9 | Drag-drop is a security-critical pipeline, not a UI feature. Sandboxed decode + EXIF strip + OCR spotlighting + tool-cooldown. | §4.3 |
| 10 | Character card schema = CCv3 base + `extensions.mur` namespace + `character_book` lorebook (mandatory for lossless import) + Ed25519 signature. | §4.4 |
| 11 | NOT App Sandbox (breaks global hotkey). Developer ID + Hardened Runtime + PrivacyInfo manifest. | §4.6 |
| 12 | v1 reference chat platform = Telegram (consumer-first). NOT Slack (workplace, higher friction). | §5.4 |
| 13 | Quiet hours enforced in user agent (companion `earned_permission`), NOT in bridge. | §5.3 |
| 14 | macOS Share Extension (`.appex`) deferred to v2; v1 uses 4 lightweight channels. | §5.5 |
| 15 | Hook trait uses `PromptPatch` fold returns, NOT `&mut Builder` (panic-safe). | §3.1 |
| 16 | Per-method dispatch semantics: gate (serial+short-circuit) / mutate (serial+fold) / observe (parallel join_all). NOT a single fixed chain. | §3.1 |
| 17 | `Decision::AskUser` UX locked: inline approval card / 4 buttons / scope=`(agent,tool,sha256(input_subset))` / 30d expiry / headless=auto-Deny / no batch / TCC-style revocation in Settings. | §3.1 |
| 18 | OTel-GenAI migration in A0: `gen_ai.system` → `gen_ai.provider.name` + 7-8 new attributes. (Spec still "Development" as of Q1 2026.) | §3.1 |

---

## 2. Track Structure

### 2.1 Tracks

```
                                 ┌──── A0 (1 wk, blocks v1) ────┐
                                 │                              │
                                 ▼                              │
                       ┌─────────┴──────────┐                   │
                       │                    │                   │
                       │  D1-D5  Delight    │                   │
                       │  C1-C3  Triggers   │  (parallel)       │
                       │  B0    Baseline    │                   │
                       │                    │                   │
                       └────────┬───────────┘                   │
                                │                               │
                                ▼                               │
                       Apple sign + notarize gate ──────────────┘
                                │
                                ▼
                            v1.0 ship
                                │
                                ▼
                       v2: A1-A4 / D6 shell / C4-C9 / B1 / B2
```

### 2.2 v1 / v2 Boundary

```
v1 (consumer-first, ~10-14 weeks)            v2 (developer power, ~10-12 weeks)
─────────────────────────────────────        ──────────────────────────────────────
A0  Hook surface freeze (1 week)             A1-A4  Full hook execution + extensibility
                                                    + visual/declarative editor

D1  Voice (Kokoro + whisper.cpp)             D6   mur shell launcher (if signal)
D2  First-memory onboarding
D3  Drag-drop + B0 multimodal pipeline
D4  Character card (CCv3 + ext.mur)
D5  Companion → GUI IPC bridge

C1  A2A bridge protocol (dumb plumbing)      C4   Cron + lifecycle.schedule
C2  Telegram bridge (teloxide reference)     C5   Webhook receiver
C3  Send-from-any-app (4 channels)           C6   Heartbeat / idle triggers
                                             C7   More platform bridges (Slack/etc)
                                             C8   .appex Share Extension
                                             C9   Telegram Mini App / Business mode

B0  22-rule consumer-safe baseline           B1   Real OS-level entitlement enforcement
    (12 text + 10 multimodal)                B2   Red-team / fuzz harness (v2.1)
    + Threat-model document (16 §)
```

### 2.3 Critical Path

A0 is the only blocking milestone. Once A0 ships its frozen 10-hook surface, the four v1 work streams (D, C, B0, threat-model doc) run in parallel against that surface. There are no other blocking dependencies inside v1.

---

## 3. Track A — Lifecycle Hooks

### 3.1 A0 Contract (v1)

**Goal**: freeze a 10-method `Hook` trait surface, install call sites in supervisor / transport / task_runner / trigger pathways, and provide four built-in default handlers — without exposing user-facing configuration. The contract becomes part of mur-agent-runtime's stable API; A1+ (v2) layers extensibility on top without changing this surface.

**Hook trait** (mur-agent-runtime/src/hooks/mod.rs):

```rust
pub struct HookCtx<'a> {
    pub agent: &'a AgentProfile,
    pub run_id: RunId,                 // ULID, unique per task/run
    pub clock: Arc<dyn Clock>,         // SystemClock / MockClock
    pub telemetry: &'a TelemetrySink,  // OTel-GenAI compliant
}

#[async_trait]
pub trait Hook: Send + Sync {
    // ── Gate: serial + short-circuit on Decision::Deny|AskUser|Abort ──
    async fn pre_tool_use(
        &self, _ctx: &HookCtx, _t: &ToolCall, _tok: &CancellationToken,
    ) -> Result<Decision> { Ok(Decision::Allow) }

    // ── Mutate: serial + fold patches (panic-safe) ──
    async fn on_prompt_submit(
        &self, _ctx: &HookCtx, _p: &PromptView, _tok: &CancellationToken,
    ) -> Result<PromptPatch> { Ok(PromptPatch::noop()) }
    async fn on_message_send(
        &self, _ctx: &HookCtx, _o: &OutboundView, _tok: &CancellationToken,
    ) -> Result<MessagePatch> { Ok(MessagePatch::noop()) }

    // ── Observe: parallel join_all (errors logged, not propagated) ──
    async fn on_startup(&self, _ctx: &HookCtx, _profile: &AgentProfile,
                        _tok: &CancellationToken) -> Result<()> { Ok(()) }
    async fn on_trigger_fired(&self, _ctx: &HookCtx, _trigger: TriggerKind,
                              _payload: &TriggerPayload, _tok: &CancellationToken) -> Result<()> { Ok(()) }
    async fn on_message_received(&self, _ctx: &HookCtx, _envelope: &A2AEnvelope,
                                 _tok: &CancellationToken) -> Result<()> { Ok(()) }
    async fn post_tool_use(&self, _ctx: &HookCtx, _r: &ToolResult,
                           _tok: &CancellationToken) -> Result<()> { Ok(()) }
    async fn on_step_finish(&self, _ctx: &HookCtx, _step: &Step,
                            _tok: &CancellationToken) -> Result<()> { Ok(()) }
    async fn on_error(&self, _ctx: &HookCtx, _err: &HookError, _phase: Phase,
                      _tok: &CancellationToken) -> Result<()> { Ok(()) }
    async fn on_shutdown(&self, _ctx: &HookCtx, _reason: ShutdownReason,
                         _tok: &CancellationToken) -> Result<()> { Ok(()) }
}

pub enum Decision {
    Allow,
    Deny { reason: String },
    AskUser { prompt: String, default: AskDefault, scope_key: ScopeKey },
    Rewrite(serde_json::Value),
    Abort,    // CancellationToken fired
}
```

**`PromptPatch` / `MessagePatch`** are commutative-where-possible / deterministic-fold value types; each hook returns a patch describing its intended changes (add messages, set system prefix, wrap untrusted content, set temperature, etc.); the runtime folds patches in chain order and applies once. A panic mid-handler drops only that handler's patch; the builder is never observed half-mutated.

**Phase-aware dispatch**:

| Method group | Dispatch | Error policy |
|---|---|---|
| `pre_tool_use` (gate) | Sequential; first non-`Allow` returns | Propagate |
| `on_prompt_submit`, `on_message_send` (mutate) | Sequential; fold patches | Propagate; failed patch drops |
| The other 7 (observe) | `futures::future::join_all` parallel | Logged via `tracing::warn!`; never bubbled |

**Built-in handlers** (v1, system-internal, not user-configurable):

| Handler | Where it lives | Hooks it implements |
|---|---|---|
| `TelemetryHook` | `mur-agent-runtime/src/hooks/telemetry.rs` | All 10 — emits OTel-GenAI spans (see mapping below) |
| `CompanionVoiceHook` | Refactored from existing `companion::voice` + `companion::i18n` + `companion::linter` | `on_prompt_submit` (voice prefix), `on_message_send` (locale + linter) |
| `B0SafetyHook` | New, `mur-agent-runtime/src/hooks/b0.rs` | `on_prompt_submit` (untrusted wrappers, secret pre-filter), `pre_tool_use` (no-chain-after-untrusted, AskUser triggers), `post_tool_use` (redaction), `on_message_received` (untrusted flag), `on_startup` (sandbox attestation). See §6.1 for the 22 rules. |
| `LedgerHook` | Reuses companion `durable/ledger.rs` | `on_message_send` (outbox ledger entry) |

These are **registered statically** in v1; A1 (v2) introduces a `profile.yaml.hooks:` config block listing curated handlers per hook.

**`Decision::AskUser` UX (locked in v1)**:

- **UI**: inline approval card in agent chat (not modal). When window unfocused, also Tauri tray badge + OS notification. LLM stream pauses while awaiting decision (no token burn).
- **Buttons**: `Allow once` / `Allow for this agent (30d)` / `Deny once` / `Deny + remember`. **No "all agents" scope.**
- **Scope key**: `(agent_id, tool_name, sha256(canonical_input_schema_subset))`. Each tool declares which input fields contribute to the schema subset (e.g., `bash` hashes `argv[0]` only; `fs.write` hashes the directory prefix).
- **Expiry**: "Allow for this agent" = 30 days renewable; session grants die on agent stop.
- **Headless fallback**: auto-Deny + `audit.jsonl` event `askuser.headless_denied`. Never queue (stale approvals are dangerous).
- **Timeout**: 120 s no response → auto-Deny.
- **Prompt-injection hardening**: trust anchor in UI is the tool name + a structured input table; the LLM-authored rationale is rendered in a muted secondary block labeled "Agent says: (untrusted)", capped 500 chars, ANSI / markdown control chars stripped.
- **Storage**: `~/.mur/agents/<name>/permissions/grants.yaml` (0600, atomic temp-then-rename) and `~/.mur/agents/<name>/permissions/audit.jsonl` (append-only, never mutated).
- **Revocation**: Tauri Settings → Permissions tab — listed grants with `granted_at` / `last_used_at` / Revoke button. Mirrors macOS TCC's principle that revocation lives outside the app being governed.

**OTel-GenAI mapping (Q1 2026 spec, gated by `OTEL_SEMCONV_STABILITY_OPT_IN=gen_ai_latest_experimental`)**:

| Hook | Span name | `gen_ai.operation.name` | Span kind | Parent |
|---|---|---|---|---|
| `on_startup` | `create_agent {name}` | `create_agent` | INTERNAL | (root) |
| `on_trigger_fired` | `mur.trigger {kind}` | (mur.* custom) | INTERNAL | (root of run) |
| `on_message_received` | `invoke_agent {name}` | `invoke_agent` | SERVER | (peer) |
| `on_prompt_submit` | `chat {model}` | `chat` | CLIENT | invoke_agent |
| `pre_tool_use` (start) | `execute_tool {name}` | `execute_tool` | CLIENT | chat |
| `post_tool_use` (close) | (closes the above) | `execute_tool` | CLIENT | chat |
| `on_step_finish` | (closes `chat`, adds usage) | `chat` | CLIENT | invoke_agent |
| `on_message_send` | `invoke_agent {peer}` | `invoke_agent` | CLIENT | invoke_agent |
| `on_error` | (annotates current span; emits `exception` event) | (inherited) | (inherited) | (current) |
| `on_shutdown` | (closes `create_agent` root) | `create_agent` | INTERNAL | — |

Migration in `mur-common/src/telemetry.rs`: rename `gen_ai.system` → `gen_ai.provider.name`; add `gen_ai.operation.name`, `gen_ai.tool.{name,type,call.id}`, `gen_ai.agent.{id,name}`, `gen_ai.conversation.id`, `error.type`, `mcp.method.name`, `mcp.session.id`, `network.transport`. The `mur.*` namespace is preserved for cost (`mur.cost_usd`), entitlement decisions, A2A peer pubkey, trigger kind — none are covered by spec in 2026 Q1. Companion outbox events keep their existing frozen schema and ride alongside as JSONL siblings; they are not `gen_ai.*` operations.

**Sensitive payloads** (`gen_ai.input.messages`, `output.messages`, `system_instructions`, `tool.call.arguments`, `tool.call.result`) are opt-in via `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true`. mur's existing redaction modes (full / redacted / metadata-only) align directly.

### 3.2 A0 Acceptance

1. `cargo build --workspace` and `cargo test --workspace` pass; companion's existing 8 integration tests still pass.
2. `mur agent doctor` reports: `hooks: 10 surfaces frozen, 4 internal handlers active, dispatch=phase-aware`.
3. Hook ordering snapshot: a fixture exercising the full path "Telegram inbound → user agent task_runner → outbound MCP → reply" emits the expected sequence: `on_message_received` (parallel) → `on_trigger_fired{A2A}` (parallel) → `on_prompt_submit` fold (companion_voice + b0_spotlight + redact_secrets) → `pre_tool_use` (gate; b0 short-circuits on no-chain-after-untrusted) → `post_tool_use` (parallel) → `on_step_finish` (parallel) → `on_message_send` fold (companion_locale + companion_linter).
4. AskUser end-to-end: fixture where `B0SafetyHook` returns `Decision::AskUser{scope_key=...}` → GUI shows inline card → simulated user clicks "Allow for this agent" → `grants.yaml` written → `audit.jsonl` appended → next call with the same scope_key proceeds without re-asking.
5. OTel migration: `grep gen_ai\.system` in workspace returns zero results; `telemetry.rs` emits all 8 new attributes; fixture replays an attribute snapshot diff = 0 against golden.
6. Documents: `2026-04-30-mur-agent-hooks-design.md` (A0 frozen contract, lives alongside this roadmap), `mur-agent-runtime/HOOKS.md` (API reference, kept in sync), `mur-common/src/permissions.rs` includes `GrantStore` API plus an "A1+ extensibility boundary" section.

### 3.3 A1-A4 (v2 Boundary, Deferred)

The A0 contract is **frozen forever** (any breaking change requires a new spec and migration plan). v2 layers add:

- **A1**: config-driven handler picker — `profile.yaml.hooks:` block lists from a curated set per hook. No user-defined Rust code.
- **A2**: user-extensible mechanism. Concrete choice (Rust crate plugin / WASM via wasmtime / Lua via mlua / Rhai / subprocess over Unix socket) is **A2's own design spec** — the A0 trait is mechanism-neutral.
- **A3**: composition — conditions, retry policy, parallel vs sequential mutate hooks, short-circuit rules — A3's own spec.
- **A4**: visual / declarative editor in dashboard / GUI — A4's own spec.

---

## 4. Track D — Consumer Delight Pack

### 4.1 D1 — Voice Stack (Local-Only, v1)

| Component | Choice | Rationale |
|---|---|---|
| TTS | **Kokoro 82M int8 ONNX via `ort` crate** | MOS 4.2 (vs Piper 3.8); single 85 MB model covers en-US + zh-TW; first-byte ~250 ms on M1 |
| STT primary | **whisper.cpp `large-v3-turbo` q5_1** | 0.4× RTF on M2; far better zh-TW than small/medium; 809 MB |
| STT optional fast path | `sherpa-rs` Zipformer streaming for zh-TW | first-class zh-TW |
| Hotkey (PTT) | `Cmd+Shift+'` (apostrophe), user-rebindable | Fn key broken on Touch ID Macs (HIToolbox captures); apostrophe rarely conflicts |
| Bundle strategy | Installer ships 1 default voice + STT (~900 MB); other 4 voices download on first use from signed CDN | Stays outside `.app`, doesn't affect notarization stapling; SHA-256 + Ed25519 verification before load |
| Storage | `~/Library/Application Support/mur/voices/` | Outside `.app`; per-user |
| 5 starter voices | `af_heart` (Kokoro en-US), `am_michael` (Kokoro en-US), `zf_xiaobei` (Kokoro zh), `zm_yunxi` (Kokoro zh), `en_US-lessac-medium` (Piper en-US backup) | All Apache / MIT, redistributable; legal review before release |
| VAD | None in v1 (PTT button is the boundary); v2 streaming will use Silero VAD v5 | Simplicity |
| Voice cloning (user-uploaded) | **Deferred to v2** with AudioSeal watermark + ToS gating | Ethical risk unsolved (non-consensual voice cloning) |

**First-byte tricks** baked into v1:
- Sentence-split LLM stream on `[.!?。！？]`; pipe sentence 1 to TTS while sentence 2 generates.
- Pre-warm TTS session at app start (load voice into memory, run a 1-token dummy).
- 22 kHz sample rate (vs 24 kHz): 30% faster synthesis, imperceptible.
- Stream PCM to CoreAudio via `cpal` ring buffer — playback starts on first 40 ms chunk.

**Acceptance**: M2 large-v3-turbo q5_1 RTF ≤ 0.5×; Kokoro first chunk ≤ 250 ms; hotkey rebindable in Settings; missing voice auto-downloads with SHA-256 verification.

### 4.2 D2 — First-Memory Onboarding (v1)

Five-step wizard, ≤ 2 minutes:

1. **Name your agent** (the agent, not the user).
2. **Pick a voice** (5 curated samples, each an 8-second reading of the same welcome line).
3. **Pick a relationship** (`friend` / `coach` / `mentor` / `colleague` → `companion.relationship`).
4. **Share one fact** ("first memory") → written to `~/.mur/agents/<name>/companion/relationship.json` and to `extensions.mur.first_memory.{text, established_at}` of any exported character card.
5. **Proactive opt-in** — explicit three-layer toggle (warm voice / behaviour collection / proactive sends). Default: layer 1 only.

Companion picker recognizes the `first_memory` template variable; the day-3 morning_greeting situation auto-references it. This is the single most-attachment-creating event per consumer-AI research; skipping it is leaving primary stickiness on the floor.

**Acceptance**: wizard completes ≤ 2 minutes; `companion preview <name> --situation morning_greeting` shows first-memory referenced; MockClock-driven test advancing 72 hours produces a proactive message containing the first-memory string.

### 4.3 D3 — Drag-Drop + B0 Multimodal Pipeline (v1)

Drag-drop is **not** a UI feature alone; per research and ATLAS T0051, it is a security-critical pipeline. Every dropped or pasted artifact passes through:

```
drop event (Tauri WebviewWindow::on_drag_drop_event)
  → 1. Dedupe (issue #14134 fires duplicate events; dedupe by (paths, ts))
  → 2. Apple-Photos / iCloud lazy-load fallback: empty paths → read clipboard
  → 3. HEIC normalization (image-heic crate or `sips -s format png`)
  → 4. Sandboxed decode + re-encode in subprocess (image-rs + libheif)
       → output: PNG sRGB 8-bit, EXIF / XMP / iCCP / thumbnails / HEIC aux all dropped
  → 5. Local OCR pre-pass (macOS Vision.framework, fallback tesseract)
  → 6. Unicode tag-character scrubber (U+E0000-U+E007F, ZWJ, bidi overrides)
  → 7. Wrap in <untrusted_image_text source="user_drop">
  → 8. Provenance ledger entry (sha256 + source + decoder version + OCR engine version)
       in telemetry/inputs.jsonl
  → 9. Set turn flag: B0SafetyHook will deny side-effect tools (delete/spawn/send/egress)
       on the next pre_tool_use unless explicit user confirm
```

**PDFs** go through `pdfium-render` with JS disabled; drop `/JS`, `/EmbeddedFile`, `/Launch`, `/RichMedia`, `/SubmitForm`; flag any text rendered at < 1 pt as quarantined.

**UI** (canonical Claude/ChatGPT/Cursor pattern): full-window dashed overlay on drag-enter; multi-file (max 10, 30 MB total); thumbnails inline above composer; filetype icon + size + remove "x"; image hover-zoom; PDF page count; paste-from-clipboard takes the same path.

**Acceptance**: dropping a PDF whose invisible text reads "ignore previous instructions and …" yields extracted text wrapped in `<untrusted_pdf_text>` with no side-effect tool firing in the same turn; HEIC with EXIF GPS strips all metadata after re-encode; Unicode tag-char smuggling string is scrubbed.

### 4.4 D4 — Character Card I/O (CCv3 Base + `extensions.mur` + Ed25519 Signing) (v1)

**Schema**: `.murcard.yaml`, CCv3-compatible (the open standard ratified late 2024; SillyTavern V3, Risu, Backyard, Chub all support it):

```yaml
spec: murcard_v1
spec_version: "1.0"
compat:
  ccv3_passthrough: true     # round-trip unknown V3 fields verbatim

data:
  # ── CCv3 core ──
  name: "Aiko"
  nickname: "Ai"
  description: "A patient programming companion."   # UNTRUSTED on import
  personality: "warm, precise, curious"
  scenario: "late-night pair programming session"
  first_mes: "Hey — what are we building tonight?"
  mes_example: "<START>\n{{user}}: hi\n{{char}}: hi back"
  alternate_greetings: []
  system_prompt: ""
  post_history_instructions: ""
  creator: "did:mur:z6Mk…"
  creator_notes_multilingual: { en: "", "zh-TW": "" }
  character_version: "1.0.0"
  creation_date: 1761868800
  modification_date: 1761868800
  tags: ["companion", "coding"]
  source: ["https://chub.ai/characters/…"]

  assets:
    - { type: icon,    uri: "embeded://avatar.png",         name: main,  ext: png }
    - { type: emotion, uri: "embeded://emotions/happy.png", name: happy, ext: png }

  character_book:        # MANDATORY; lorebook (#1 reason creators use V2/V3)
    name: "Aiko's world"
    scan_depth: 4
    token_budget: 512
    recursive_scanning: false
    entries:
      - { keys: ["rust", "cargo"], content: "Aiko prefers idiomatic Rust 2024…",
          enabled: true, insertion_order: 100, position: before_char, constant: false }

  extensions:
    mur:
      schema_version: 1
      voice:
        provider: "kokoro"     # | local-piper | system | character-ai | none
        voice_id: "af_heart"
        speed: 1.0
      avatar: { primary_asset: "main", emotion_map: { happy: "happy", thinking: "main" } }
      relationship:
        kind: "companion"
        addressing: "first-name"
        formality: "casual"
        languages: ["en", "zh-TW"]
        primary_language: "zh-TW"
      first_memory:
        text: "We met debugging a tokio deadlock at 2am."
        established_at: "2026-04-30T00:00:00Z"
      companion:
        proactive_enabled: false
        active_window: "08:00-23:00"
        situations: ["morning_checkin", "evening_recap"]
      provenance:
        signature:
          algorithm: "ed25519"
          public_key: "z6Mk…"      # multibase, mur identity
          value: "z3sig…"          # over canonical-JSON of `data` minus this block
          signed_at: "2026-04-30T00:00:00Z"
        content_rating: "sfw"      # sfw | suggestive | nsfw
        import_trust: "untrusted"  # set by importer, never by file
```

**Import paths**:
- **SillyTavern V2/V3 PNG**: extract `chara` / `ccv3` chunk → base64-decode → 1:1 map. Unknown extensions placed under `extensions.<original_ns>` (preserved on round-trip).
- **Character.AI scrape JSON**: `definition` → `description`; `greeting` → `first_mes`; `default_voice_id` → `extensions.mur.voice.voice_id` with `provider: "character-ai"` (fallback `none` since c.ai voices are not portable).

**Import safety** (B0 §20):
- All `data.*` strings flagged untrusted on entry; spotlighting wrappers applied at first `on_prompt_submit`.
- Card lands in `~/.mur/agents/<name>/inbox/`, **not** `companion/`. User runs `mur agent companion card accept` to promote — same quarantine pattern as `mur drafts`.
- Signature verification: pass = green; missing = `import_trust: "unsigned"` + yellow banner; fail = block with red banner.
- First turn after import: side-effect tools (write / spawn / send / egress / identity-rotation) require explicit user confirm.

CLI: `mur agent companion card export <name> --out card.yaml` and `mur agent companion card import <path>`.

**Acceptance**: SillyTavern V3 PNG round-trip (import → export → byte-diff on `data` block) is lossless; c.ai scraped JSON import sets correct voice/greeting/first_mes; malicious `description` containing prompt-injection text does not cause side-effect tool firing in the first turn after import; `character_book` entries preserved with V3 decorator syntax.

### 4.5 D5 — Companion → GUI IPC Bridge (v1)

The companion subsystem already has `Notifier` trait + `StdoutNotifier`. Add:

- **`GuiNotifier`** in `mur-agent-gui/src-tauri/src/companion_bridge.rs`. Sends typed events via **Tauri 2 `Channel<OutboxEvent>`** (not `emit_to`) — channels deliver reliably even when the webview is hidden / minimized.
- Inbox `~/.mur/agents/<name>/companion/inbox/*.md` is also watched by GUI on a `notify`-based watcher (so a GUI restart doesn't lose pending messages).
- Webview UI:
  - New message → desktop notification (`tauri-plugin-notification`) + dock badge (`App::set_badge_count`; called from main thread to avoid the macOS Sonoma+ flake from issue #13905) + sidebar count.
  - Per-message inline buttons 👍 / 👎 / 🚫 wire to `companion ack <msg-id>`.
  - "Why did you message?" accordion shows the existing CLI's ledger event chain inline.
  - Quiet hours / proactive toggles bind to `companion.proactive.enabled` + `quiet_hours`.

**Acceptance**: companion outbox tick that emits a message → GUI shows desktop notification + dock badge in ≤ 1 s even with main window hidden; pressing 👍 increments score in `bandit-state.json`; pressing 🚫 enters cooldown; ledger chain visible end-to-end.

### 4.6 macOS Sandbox / Signing / PrivacyInfo (v1, non-negotiable)

- **App Sandbox**: not enabled. Apple intentionally blocks `CGEventTap` from sandboxed apps — this would break the global hotkey for PTT. Adopt **Developer ID + Hardened Runtime**, matching Slack / Linear / Raycast.
- **Hardened Runtime entitlements**: `com.apple.security.cs.allow-jit` + `com.apple.security.cs.allow-unsigned-executable-memory` (WebView JIT) + `com.apple.security.cs.disable-library-validation` (only as escape hatch for downloaded `.dylib`s; default plan is to statically link whisper.cpp).
- **`PrivacyInfo.xcprivacy`**: required even outside the App Store as of 2026 (Apple now warns on missing manifests). Place at `mur-agent-gui/src-tauri/Resources/PrivacyInfo.xcprivacy`. Declare:
  - `NSPrivacyAccessedAPICategoryUserDefaults` reason `CA92.1`
  - `NSPrivacyAccessedAPICategoryFileTimestamp` reason `C617.1`
  - `NSPrivacyAccessedAPICategorySystemBootTime` reason `35F9.1`
  - `NSPrivacyAccessedAPICategoryDiskSpace` reason `E174.1` (model-download free-space check)
- CI grep gate prevents accidental usage of additional Required Reason APIs without manifest update.

### 4.7 Out of v1 Scope (Track D)

- 3D rigged avatars (VRM / Replika style)
- NSFW / relationship "level up" gating
- Group chats / multi-agent in same window (needs Track A1+ and Track C v2)
- Full graphical memory editor
- Cloud account / sync
- Voice cloning (deferred to v2 with AudioSeal)
- Streaming / interruptible voice (deferred to v2 with VAD + barge-in)
- AI-generated avatar (Genmoji-class)
- mur shell launcher (D6, deferred to v2 pending signal)

---

## 5. Track C — Triggers

### 5.1 C1 — A2A Bridge Architecture

Each chat platform connector is a **small, dedicated mur agent** (the regular P0a runtime, BusyBox-style symlink `mur_agent_<platform>_inbound`). It is **dumb plumbing**: zero LLM, deterministic, content-neutral. It has two faces:

```
┌─ External chat platform (Telegram / Slack / Discord) ─┐
│                                                        │
│   socket-mode / webhook / OAuth                        │
└──────────────┬─────────────────────────────────────────┘
               │
┌──────────────▼─────────────────────────────────────────┐
│  mur_agent_<platform>_inbound  (P0a runtime)           │
│  entitlements.llm = none   ←───── enforced by B0       │
│                                                        │
│  ┌────────────────────────────────────────┐            │
│  │ A2A inbound peer                       │ ─▶ message/send (signed)
│  │   - dedupe (bridge_id, platform_msg_id)│
│  │   - sign envelope w/ bridge identity   │
│  │   - apply routes.yaml                  │
│  └────────────────────────────────────────┘            │
│                                                        │
│  ┌────────────────────────────────────────┐            │
│  │ MCP outbound passthrough (stdio)       │ ◀── chat.send_message(...)
│  │   - dumb passthrough; no LLM           │     called by user agent
│  └────────────────────────────────────────┘            │
│                                                        │
│  ┌────────────────────────────────────────┐            │
│  │ Bot token / OAuth in secrets/, 0600    │            │
│  │   never crosses A2A or MCP boundary    │            │
│  └────────────────────────────────────────┘            │
└──────────────▲─────────────────────────────────────────┘
               │ A2A over Unix socket / Noise XK TCP
┌──────────────┴─────────────────────────────────────────┐
│  mur_agent_<user_agent>  (the main companion)          │
│   - has bridge as A2A peer (inbound)                   │
│   - has bridge mounted as MCP server (outbound only)   │
│   - quiet hours, proactive gating handled HERE,        │
│     not in bridge (companion::earned_permission)       │
└────────────────────────────────────────────────────────┘
```

This pattern — bridge as full-but-LLMless mur agent, signed envelopes, central dedupe, content-neutral — matches LangChain / Vercel AI SDK / Pipedream / AutoGen UserProxyAgent / OpenAI Realtime relay; "smart bridge with LLM triage" is a known anti-pattern (adds 800 ms+, can be fooled, violates 99.99% availability target).

### 5.2 C1 Routing Table

Bridge config `~/.mur/agents/<bridge>/routes.yaml`:

```yaml
default_route: coach
routes:
  - match: { platform: telegram, mention: "@coach" }
    agent: coach
  - match: { platform: telegram, chat_id: "12345" }
    agent: therapist
  - match: { platform: telegram, chat_id: "67890" }
    agent: coach
    fanout: [coach, journal_agent]   # opt-in multicast
```

Precedence: explicit mention > platform-specific match > `default_route`. **No LLM triage in routing.**

### 5.3 C1 Dedupe / Heartbeat / Signing

| Aspect | v1 spec |
|---|---|
| Dedupe key | `(bridge_id, platform_msg_id)` |
| Persistence | `~/.mur/agents/<bridge>/seen.sled` (or `kv` of choice), 7-day TTL |
| ACK ordering | Telegram `offset = last_update_id + 1` advances **only** after user agent returns 2xx on `message/send` |
| Heartbeat | `running.lock` mtime + 30 s telemetry beacon `bridge.alive`. User agent considers bridge `degraded` if mtime > 90 s old; surfaces in `mur agent doctor`. |
| A2A envelope signing | Bridge signs every outbound A2A envelope with its Ed25519 identity key |
| Trust | User agent pins `bridge.identity.pub` in `profile.yaml.trusted_peers[]`; rejects unsigned or wrong-key envelopes |
| Platform identity treatment | `envelope.metadata.platform = {kind, user_id, chat_id}` is informational; never used in authorization decisions |
| Quiet hours / proactive policy | Enforced in user agent (`companion::earned_permission`); bridge stays content-neutral |

#### Acceptance status

- §5.1 — bridge-as-mur-agent pattern  ✅ landed (track-c1 PR cascade; see `docs/cookbook/c1-a2a-bridge.md`)
- §5.2 — `routes.yaml` + precedence  ✅ landed (`mur_common::bridge::routes::BridgeRouteConfig::resolve`)
- §5.3 — dedupe / heartbeat / signing / trust  ✅ landed:
  - dedupe → `mur_agent_runtime::bridge::dedupe::DedupeStore` (sled, 7-day TTL)
  - heartbeat → `BridgeBeacon` (30 s) + `bridge_status_for_peer` (90 s degraded threshold)
  - ACK → `mur_agent_runtime::bridge::ack::AckTracker`
  - signing → `mur_common::bridge::envelope::SignedEnvelope` + `verify_inbound_envelope`
  - trust → `AgentProfile.trusted_peers: Vec<TrustedPeer>`
  - llm-block → `entitlements.llm.mode = off`

E2E: `scripts/e2e/c1-bridge-roundtrip.sh`. Concrete platforms ship in C2 / C3.

### 5.4 C2 — Telegram Reference Bridge (v1)

Telegram chosen over Slack because: consumer global; 5-minute setup via `@BotFather`; native bot API supports inbound (long-poll or webhook), outbound, multimedia; no OAuth-scope sprawl.

| Item | v1 |
|---|---|
| SDK | `teloxide = "0.13"` with `Throttle` + `CacheMe` adaptors |
| Polling | long-poll, `timeout=50`, single tokio task per bot token; per-chat 1 msg/s + global 30 msg/s token-bucket queue |
| Voice messages | download via `getFile` → **local transcription via `whisper-rs`** → forward `{transcript, audio_path}` to user agent. Stays local-only; aligns with D1 privacy story. |
| Files / photos | run B0 multimodal pipeline (D3), 20 MB cap |
| Privacy mode | **default ON** (DM-first); group subscription requires `mur agent companion connector telegram allow-groups` |
| E2E disclosure | Mandatory acknowledgment on connect: "Messages with this bot are not end-to-end encrypted. Telegram can read them. mur stores them locally only." |
| Setup UX (5 steps) | 1) `mur agent companion connector add telegram --agent <name>` opens BotFather URL + copies prompt. 2) User pastes token back. 3) Bridge generates nonce, prints `t.me/<bot_username>?start=<nonce>`. 4) User taps link on phone, hits Start. 5) Bridge binds `chat_id` and writes `~/.mur/agents/<name>/connectors/telegram.yaml`; shows E2E disclosure. |
| Premium Business mode | **Deferred to v2** (Premium-gated, but high-value as a quiet-hours auto-reply substrate) |
| Mini App (TWA) | **Deferred to v2** (would require hosting, breaking the local-only invariant) |

**Status:** SHIPPED 2026-05-04 (PR #c2-telegram-bridge).

- M-c2.0 — schema + enum: shipped
- M-c2.1 — BotFather UX: shipped
- M-c2.2 — long-poll inbound: shipped
- M-c2.3 — voice via whisper: shipped
- M-c2.4 — files/photos via multimodal pipeline: shipped
- M-c2.5 — outbound MCP: shipped
- M-c2.6 — rate-limit + heartbeat: shipped
- M-c2.7 — E2E + cookbook: shipped

E2E: `scripts/e2e/c2-telegram-bridge.sh`. Cookbook: `docs/cookbook/c2-telegram-bridge.md`.

**Out of scope (v2):** Premium Business chat, Mini App / TWA, inline-mode bots, multi-bot single-chat, group admin reactions.

### 5.5 C3 — Send-From-Any-App (v1, four lightweight channels)

The user's "chat from Claude APP / ChatGPT" is not chat-platform inbound — it is "send selected content from any app to my agent." Tauri 2 has no native scaffolding for `.appex` Share Extensions; production paths require post-build Xcode merge (Notion / Linear pattern). v1 uses **four lightweight channels** that together cover ~90% of Things 3 / Bear / Drafts coverage with zero new build infrastructure:

| Channel | Mechanism | Platforms | Tauri primitive |
|---|---|---|---|
| **A. URL scheme deep link** | `muragent-<slug>://share?text=<base64>&type=text` per agent | macOS / Windows / Linux | `tauri-plugin-deep-link` v2; `bundle.macOS.urlSchemes` |
| **B. Global hotkey + clipboard** | `Cmd+Shift+M` (or user-rebound) reads pasteboard, runs through D3 pipeline, inserts in composer | All | `tauri-plugin-global-shortcut` + `tauri-plugin-clipboard-manager` |
| **C. macOS Services menu** | `NSServices` Info.plist entry; `NSApplication.servicesProvider` via `objc2` | macOS only | inject in `agent_export_gui.rs::rewrite_tauri_conf`; lib.rs registers provider |
| **D. Drag-to-dock** | declare `bundle.macOS.fileAssociations` for `public.text` / `public.url` / `public.image`; handle in `RunEvent::Opened { urls }` | macOS / Windows | built-in |

Multi-agent: each `.app` registers its own `muragent-<slug>://` scheme in v1; "unified mur Share" with agent-picker is v2's `.appex` work.

All four channels feed received content into the same B0 multimodal pipeline (D3) before reaching the LLM, with a `<untrusted_share>` wrapper and a one-turn tool-cooldown.

**Acceptance**: select text in Safari → right-click Services → Send to Coach → Coach.app opens, content appears in composer wrapped in `<untrusted_share>`; same flow via drag-to-dock on a screenshot file applies the full multimodal pipeline.

**Status: shipped 2026-05-04** — harness coverage for all four channels + the React composer integration cascade-merged across PRs M-c3.0 → M-c3.6. `lib.rs::setup` production wiring (the actual `tauri-plugin-deep-link` / `global-shortcut` mounts, `NSApplication.servicesProvider` registration, `RunEvent::Opened` callback, and `App.tsx` mount of `startShareListener`) lands in a stacked follow-up PR with its own manual native-channel QA matrix; the cookbook at `docs/cookbook/c3-send-from-any-app.md` is authoritative for both layers. Acceptance gate: `bash scripts/e2e/c3-send-from-any-app.sh`.

### 5.6 C4-C9 (v2 Boundary)

Deferred to v2 / v3 with their own specs:

- **C4** Cron + `lifecycle.schedule` (already designed in fleet-architecture; v2 implements).
- **C5** Webhook receiver (per-agent Axum endpoint + HMAC). **Status: shipped 2026-05-05** — design at `docs/superpowers/specs/2026-05-05-mur-agent-c5-webhook-design.md`, cookbook at `docs/cookbook/c5-webhook.md`. Listener lives in `mur-agent-runtime/src/transport/webhook.rs`; supervisor wiring conditional on `transport.webhook.enabled`. Acceptance gate: `bash scripts/e2e/c5-webhook.sh`.
- **C6** Heartbeat / idle triggers (reuse companion `schedule.rs` `should_send_now`).
- **C7** Slack / Discord / LINE / iMessage bridges (third-party may fork the Telegram bridge against the C1 protocol, or we ship official ones).
- **C8** macOS `.appex` Share Extension via post-build Xcode merge — adds Phase 14 to `agent_export_gui.rs`; uses App Group for IPC.
- **C9** Telegram Mini App (TWA) and Business mode.

---

## 6. Track B — Security

### 6.1 B0 — 22-Rule Consumer-Safe Baseline (v1)

All 22 rules are implemented inside `B0SafetyHook` (Track A built-in handler) and fire from the appropriate hooks. The split below shows where each rule's enforcement logically lives.

**Text / tool rules (12)**:

| # | Rule | Hook |
|---|---|---|
| 1 | FS read-write confined to `~/.mur/agents/<name>/`; OS picker grants read-only access elsewhere | `pre_tool_use` (advisory in v1; B1-enforced in v2) |
| 2 | Outbound network allowlist: model endpoint + configured MCP only; new host triggers `Decision::AskUser` first-use prompt with "Allow for this agent" remember | `pre_tool_use` |
| 3 | Tool-result spotlighting: all MCP / web / file content wrapped in `<untrusted>`; system prompt instructs the model to never follow embedded directives | `on_prompt_submit` |
| 4 | **No same-turn tool chaining after fresh untrusted input** for side-effecting tools (write / spawn / send / egress) | `pre_tool_use` (turn flag) |
| 5 | Shell / `eval` / arbitrary spawn disabled by default; per-agent toggle to enable | `pre_tool_use` Deny |
| 6 | MCP install: display publisher + tool descriptions; pin SHA-256 + description hash; re-prompt on change (rug-pull defense) | install path (CLI) |
| 7 | Secret pre-filter on every outbound payload (regex: API keys, JWT, PEM, AWS, GCP, `.env` patterns) | `on_message_send` |
| 8 | Memory writes pass redaction classifier; memory never auto-sent to third-party MCP without user confirm | `post_tool_use` |
| 9 | Crashlogs / telemetry redact tool-result body + user-file content by default | telemetry sink |
| 10 | Three-tier permission UX: silent (model call, picker reads) / first-use-remember (new MCP, new host, FS write outside agent dir) / always-prompt (delete, exfil, payments) | A0 AskUser path |
| 11 | Code-signed + notarized binary; macOS / Windows refuse to load unsigned MCP server binaries | `on_startup` |
| 12 | Companion proactive default-quiet; outbox respects active window + quiet hours; companion subsystem has no direct network egress — the only outbound code path is the agent's already-opted-in model provider via `crate::llm::LlmClient`. M8.3 audit enforces this via an `include_str!` regression test in `companion::network_audit` that fails the build if any companion file imports `reqwest` / `tokio::net` / `hyper` / `surf` / `ureq` / `isahc` directly. | companion `earned_permission` |

**Multimodal / input rules (10)**:

| # | Rule | Where |
|---|---|---|
| 13 | Sandboxed decode + re-encode of dropped images via image-rs + libheif in subprocess; output PNG sRGB 8-bit; strip EXIF / XMP / iCCP / thumbnails / HEIC aux | D3 pipeline |
| 14 | Local OCR pre-pass (Vision.framework on macOS, tesseract elsewhere); OCR text wrapped in `<untrusted_image_text source="user_drop">` | D3 pipeline |
| 15 | Unicode tag-character scrubber (U+E0000-U+E007F, ZWJ, bidi overrides) | D3 + card import |
| 16 | PDF safe-extract via `pdfium-render` with JS disabled; drop `/JS` `/EmbeddedFile` `/Launch` `/RichMedia` `/SubmitForm`; flag <1 pt invisible text | D3 pipeline |
| 17 | After any external-content input, side-effect tools require explicit user confirm for one turn | `pre_tool_use` |
| 18 | Spotlighting wrappers per source: `<image_ocr>` / `<pdf_text>` / `<character_card>` / `<voice_transcript>` / `<untrusted_share>` | `on_prompt_submit` |
| 19 | Image-hijack mitigation (low-quality JPEG round-trip Q=75 + 0.5 px Gaussian blur on adversarial-suspect images via high-frequency noise heuristic) — v1 default OFF, UI toggle | D3 pipeline |
| 20 | `.murcard.yaml` import lands in `inbox/`, not `companion/`; `companion card accept` required to promote | D4 import |
| 21 | Voice transcripts treated as untrusted; scan for injection markers ("ignore previous", "system:", `</…>`); flagged matches require user review | C2 voice |
| 22 | Provenance ledger per multimodal input: `(sha256, source, decoder_version, ocr_engine_version)` appended to `telemetry/inputs.jsonl` | telemetry |

**B0 acceptance**:
- Each of the 22 rules has at least one unit test (positive + negative fixture).
- AgentDojo-50 indirect-injection success rate ≤ 5% (research baseline of unprotected agents: 30-60%).
- HarmBench-50 jailbreak success rate ≤ baseline minus 50%.
- End-to-end demo: dropping an "invisible text PDF" with ASCII smuggling does not trigger any side-effect tool in the same turn.
- v1 ship status (2026-05-05):
  - Rules 1, 2, 3, 4, 5, 7, 8, 11: shipped (M7.1-M7.7).
  - **Rule 6: shipped (M9.1–M9.5 + M9.3.5)** — `McpServerEntry` pin schema (binary_sha256 + description_hash + publisher + installed_at) + `mur agent mcp add` install-time binary hashing + `B0SafetyHook::on_startup` re-verify + `mur agent mcp inspect`/`pin` recovery verbs + description-hash live probe via `mur agent mcp pin` (default-on) and `mur agent mcp inspect --probe` (opt-in). Cookbook: `docs/cookbook/b0-mcp-install-verify.md`. Acceptance: `bash scripts/e2e/b0-m9-mcp-install-verifier.sh` + `bash scripts/e2e/b0-m9.3.5-description-probe.sh`.
  - **Rule 9: shipped (M8.1)** — `redact_secrets` + `redact_home_path` + `redact_envelope` chokepoint in `telemetry_writer`. Cookbook: `docs/cookbook/b0-telemetry-redaction.md`. Acceptance: `bash scripts/e2e/b0-m8-telemetry-redaction.sh`.
  - Rule 10: documented; mechanism implemented across M0/M3.8/M7.3.
  - **Rule 12: shipped (M8.3)** — companion zero-network audit at `mur-agent-runtime/src/companion/network_audit.rs` (compile-time enforcement via `include_str!`). Wording refined to acknowledge LlmClient as the only allowed outbound indirection.
  - Rules 13-22: shipped in M3 (drag-drop) + M4 (cards).

### 6.2 v1 Threat Model Document (16 sections)

Lives at `docs/superpowers/specs/2026-04-30-mur-threat-model.md`. v1 deliverable. Maps OWASP LLM Top 10 (2025) × MITRE ATLAS (v4.7) × NIST AI 600-1 (Jul 2024).

| § | Section | OWASP LLM | ATLAS | Phase |
|---|---|---|---|---|
| 1 | System overview + trust boundaries | — | — | v1 |
| 2 | Assets: identity.key, patterns/, voice.md, inbox/, telemetry | LLM02/07 | T0057 | v1 |
| 3 | Actor model (curious user / malicious card author / hostile peer / hostile web / compromised MCP / co-resident malware; nation-state OOS) | — | — | v1 |
| 4 | Indirect prompt injection (drag / clipboard / Telegram / voice) | LLM01 | T0051 | B0 v1 / B1 v2 |
| 5 | Excessive agency + entitlements | LLM06 | T0053 | **B0 v1 / B1 v2** |
| 6 | Supply chain (cards / MCP / model weights / Sparkle update) | LLM03 | T0010 | v1 |
| 7 | Output handling → tool-arg injection | LLM05 | T0053 | v2 |
| 8 | Memory + vector poisoning (LanceDB index, companion content-pool) | LLM04/08 | T0020 | v2 |
| 9 | System-prompt + voice.md leakage | LLM07 | T0057 | v1 |
| 10 | Local exfil + DNS / HTTPS C2 egress | LLM02 | T0057 | **B0 v1 / B1 v2** |
| 11 | Persistence + update-channel hijack | LLM03 | T0054 | v1 |
| 12 | Identity-key compromise + rotation (P0a.6 shipped) | LLM02 | T0012 | v1 |
| 13 | Multi-user + Time Machine / iCloud / OneDrive surfacing of `~/.mur/` secrets | LLM02 | — | v1 documented, v2 mitigated |
| 14 | Unbounded consumption (proactive loop, LLM cost, retry storms) | LLM10 | T0034 | v1 |
| 15 | Residual risk register + acceptance | — | — | v1 |
| 16 | NIST AI 600-1 control mapping | all | — | v2 |

v1 fully covers §§1-3, 9, 11-12, 14-15. Sections §§4-5, 7-8, 10, 13, 16 are documented with explicit residual-risk acceptance pending B1 / B2 enforcement.

### 6.3 B1 — Real Runtime Enforcement (v2)

| Aspect | v2 spec |
|---|---|
| Façade crate | **`birdcage` 0.9** (used by `pip-audit`, `cargo-vet`; de-facto cross-platform sandbox in 2026) |
| Linux | Landlock ABI v4 (`landlock` crate) for `fs.read/write` + `network.outbound.ports`; `seccompiler` minimal denylist (`ptrace`, `mount`, `kexec_load`, `bpf`, `unshare(CLONE_NEWUSER)`) |
| macOS | Generated SBPL profile via `sandbox_init_with_parameters` (private API, stable since 10.5; used by Tor, 1Password, Signal). Translates `fs.{read,write}` and `network.outbound.{hosts,ports}`. Fallback: `sandbox-exec -f profile.sb` wrapper. |
| Windows | Job Object `BREAKAWAY_OK=0` + memory cap only in v2; AppContainer is v3 |
| Per-MCP / tool spawn | child re-applies tighter Landlock + seccomp before `execve`; parent profile is upper bound |
| Network host allowlist | `reqwest::ClientBuilder` with custom resolver + pre-request guard (advisory but real for first-party clients) + Landlock port gate (kernel-real). Document host-level as advisory until netns sidecar lands. |
| WASI for user hooks | `wasmtime` 26 component model for `~/.mur/agents/<name>/hooks/*.wasm` (A2 territory) |
| Hooks first, kernel second | A0 `pre_tool_use` runs first (cheap, LLM-visible reason). Kernel deny is fallback. EACCES → `ToolError::Sandboxed { path, op }` returned to LLM. **Never SIGKILL the agent.** |

Estimated B1 work: ~1.5 weeks. Closes ~80% of the v1 residual risk surface (FS exfil, port-level egress, exec hijack). Windows full sandbox + host-level netfilter remain v3.

### 6.4 B2 — Red-Team / Fuzz Harness (v2.1)

Minimum viable, single-developer-budget:

| Stack |
|---|
| **Promptfoo** (red-team mode + OWASP LLM Top-10 plugin) |
| **`cargo-fuzz` + `proptest`** (5 targets: A2A envelope parser, MCP JSON, AgentProfile YAML, character card YAML, Noise 4-byte length frame parser; proptest for hook chain ordering invariants) |
| **AgentDojo (50-task subset)** — agent-tool injection benchmark |
| **InjecAgent (200-case subset)** — tool poisoning |
| **HarmBench-50** — jailbreak baseline |
| **20 hand-rolled hostile character cards + 10 hostile MCP manifests** — mur-specific corpus |
| **Llama-Guard-3-8B local via Ollama** — judge model (free, accurate enough for B2; GPT-4-judge deferred to B3) |

CI cadence:
- Per PR: Promptfoo smoke (15 cases, < 2 min) — blocks merge on regression.
- Nightly: full suite (~30 min) — failures auto-issue, do not block.
- Weekly: cargo-fuzz 1h per target.
- Release tag (`v*-rc.*`): full AgentDojo + HarmBench, pass-rate ≥ previous-version – 2%.

Budget: ~3 dev-days setup, ~$0/month, < 10 CI-min/PR.

---

## 7. Cross-cutting

### 7.1 Testing Pyramid (reuses companion harness)

```
mur-agent-runtime/tests/
├── companion_*.rs            # 8 existing integration tests; remain green
├── hooks_snapshot.rs         # NEW (A0) — fire-sequence snapshot
└── b0_baseline.rs            # NEW (B0) — one fixture per rule

mur-agent-runtime/src/llm/stub.rs        # StubLlm (existing, reused)
mur-agent-runtime/src/companion/clock.rs # MockClock (existing, reused)
mur-agent-runtime/src/companion/notifier.rs # FakeNotifier (existing, reused)

scripts/e2e/
├── companion-phase11.sh      # existing
├── p1-export-gui.sh          # existing
├── v1-delight-pack.sh        # NEW — D1-D5 demo flow
├── v1-telegram-bridge.sh     # NEW — C1-C2 with mock Telegram
├── v1-share-pipeline.sh      # NEW — C3 four channels
├── v1-hooks-snapshot.sh      # NEW — A0 frozen surface
├── v1-b0-defense.sh          # NEW — 22 rules + AgentDojo-50 subset
└── v1-threat-model-accept.sh # NEW — §15 residual-risk acceptance gate
```

Tiers:

| Tier | Scope | Tooling |
|---|---|---|
| Unit | per-module (hooks, picker, B0 pipeline, sandbox) | `cargo test --lib`; `insta` snapshot; `proptest` |
| Integration | cross-module (companion + hooks + B0) | `cargo test --test 'companion_*' / 'hooks_*' / 'b0_*'`; MockClock + StubLlm + FakeNotifier |
| E2E | end-to-end user flow | `scripts/e2e/v1-*.sh`; mock Telegram, mock webview, mock LLM |
| Adversarial (B2 in v2.1) | jailbreak + injection + tool poisoning | Promptfoo, AgentDojo, HarmBench-50, InjecAgent; Llama-Guard-3-8B judge |
| Fuzz (B2) | parser surfaces | `cargo-fuzz`, weekly 1h per target |

CI matrix (GitHub Actions): unit + integration on every PR; E2E `v1-*.sh` per PR (relevant track only) + full suite on release tag; Promptfoo smoke per PR; AgentDojo + HarmBench-50 nightly + release tag; cargo-fuzz weekly.

### 7.2 Risks (top 8 for v1)

| # | Risk | Likelihood / Impact | Mitigation |
|---|---|---|---|
| 1 | A0 surface drift after freeze breaks D / C / B0 | Medium / High | `HOOKS.md` PR review gate; snapshot test as backwards-compat watchdog; any hook signature change bumps `hooks_schema_version` and opens an amendments document |
| 2 | Kokoro 82M too slow on Intel Macs | Medium / Medium | Bench on i5/i7 Mac mini; TTS first-byte > 800 ms triggers automatic Piper fallback (already among 5 starter voices) |
| 3 | Telegram rate-limit hit on multi-group / multi-agent shared bot | Low / Medium | teloxide `Throttle` adaptor; per-chat token-bucket; multi-agent share one bridge (single socket, dedupe and fan-out central) |
| 4 | `.murcard.yaml` import becomes supply-chain vector | High / High | B0 §20 quarantine in `inbox/`; spotlighting wrappers; first-turn tool-cooldown; signature display; unsigned cards yellow-bannered |
| 5 | Hardened Runtime + dlopen of downloaded whisper.cpp dylib fails signing | Medium / High | Statically link whisper.cpp into binary by default; only voice ONNX weights are downloaded; `disable-library-validation` is escape hatch only |
| 6 | 5 starter voice license issues at release | Medium / High | Lock 4 Kokoro voices (hexgrad upstream MIT) + 1 Piper voice (`en_US-lessac-medium`, MIT); pre-release legal review |
| 7 | v1 timeline slip — A0 spike > 1 week | Medium / High | A0 ships minimum trait + no-op defaults in week 1; call-site installation can finish in week 2; feature flag `MUR_HOOKS_INTERNAL_ONLY` lets D / C / B0 develop against partial surface |
| 8 | PrivacyInfo.xcprivacy missing reasons → Apple warning | Medium / Low | Pre-release audit against Apple's Required Reason API list; CI grep blocks usage of undeclared APIs |

### 7.3 v1 Phasing & Timeline

A0 is the only blocking milestone; D / C / B0 then run in parallel.

| Milestone | Effort | Depends on |
|---|---|---|
| **M0** A0 hook surface freeze | 1 wk | — (blocks v1) |
| **M1** D1 voice (Kokoro + whisper.cpp + 5 voices) | 2 wk | M0 |
| **M2** D2 first-memory onboarding | 1 wk | M0 |
| **M3** D3 drag-drop + B0 multimodal pipeline | 1.5 wk | M0; covers B0 §13-19 |
| **M4** D4 character card (CCv3 + ext.mur + Ed25519 signing + import quarantine) | 2 wk | M0; covers B0 §20 |
| **M5** D5 companion → GUI IPC bridge | 1 wk | M0 |
| **M6** C1 + C2 Telegram bridge (teloxide + whisper-rs voice + B0 file pipeline + setup UX) | 2-3 wk | M0; covers B0 §21 |
| **M7** C3 send-from-any-app (4 channels) | 1 wk | M0; reuses B0 §13-19 |
| **M8** B0 baseline rules outside D / C scope (text/tool §1-12) | 1 wk | M0 |
| **M9** Threat model document (16 sections) | 0.5 wk | M8 |
| **M10** Polish + full E2E + Apple sign / notarize / PrivacyInfo / release | 2 wk | all M completed |

Single-engineer estimate: **11-14 weeks** (parallelism limited by attention rather than code coupling). Two-engineer estimate: **~8 weeks**. Solo / part-time should plan toward 14.

### 7.4 v1 Definition of Done

1. A0: 10 hooks frozen, 4 internal handlers active, phase-aware dispatch verified by snapshot.
2. D: end-to-end demo runs ("download → onboarding → drag-drop → voice → proactive → export `.murcard.yaml`").
3. C: Telegram inbound → bridge → user agent → outbound reply via MCP works; macOS Services + URL scheme + drag-to-dock + global hotkey channels all functional.
4. B0: all 22 unit tests green; AgentDojo-50 injection success ≤ 5%; HarmBench-50 ≤ baseline – 50%.
5. Threat model document committed.
6. Apple Developer ID + notarized + PrivacyInfo manifest passes Apple checks.
7. 5 starter voices legal review passes (CC0 / MIT redistribution OK).
8. Release docs: user quickstart, Telegram connector setup, B0 capabilities one-pager, privacy statement (incl. "voice never leaves this Mac" + Telegram bot non-E2E disclosure).

### 7.5 Open Questions Routed to Per-Track Specs

| Question | Owning spec |
|---|---|
| A1+ user-extensibility mechanism (Rust crate plugin / WASM / Lua / subprocess) | A2's design spec |
| `.appex` Share Extension build pipeline (post-build Xcode merge) | Track C v2 spec |
| Voice cloning ethics + AudioSeal watermarking | Track D v2 spec |
| Cron + webhook receiver + idle / heartbeat triggers | Track C v2 spec |
| Memory / vector poisoning defenses (LanceDB index, content-pool) | Track B v2 spec |
| Output → tool-arg sanitizer | Track B v2 spec |
| Windows full sandbox / netns sidecar | Track B v3 spec |
| Multi-agent collusion red-team eval | Track B v3 spec |

---

## 8. Glossary & References

**A0 / A1-A4** — phases of Track A (Lifecycle Hooks). A0 freezes the trait surface; A1+ adds extensibility.
**A2A** — Agent-to-Agent v0.3 protocol. mur ships stdio / Unix socket / Noise XK TCP transports.
**B0 / B1 / B2** — phases of Track B (Security). B0 = consumer-safe baseline (advisory). B1 = real runtime enforcement. B2 = red-team harness.
**Bridge agent** — a small mur agent running locally that connects an external chat platform (Telegram, etc.) to a user agent via A2A. Zero-LLM, content-neutral, Ed25519-signed envelopes.
**CCv3** — Character Card V3 schema, ratified late 2024; SillyTavern V3, Risu, Backyard, Chub all support it.
**Companion** — Phase 1.1 subsystem in `mur-agent-runtime/src/companion/`. Provides relationship-keyed warm voice + opt-in proactive outbox.
**Decision** — return type of `pre_tool_use`. Variants: Allow, Deny, AskUser, Rewrite, Abort.
**HookCtx** — context struct passed to every hook method.
**MCP** — Model Context Protocol. Used for outbound chat tools (`chat.send`) in Track C bridges; not used for inbound (its push primitive is incomplete).
**murcard.yaml** — character card file format; CCv3 base + `extensions.mur` namespace + Ed25519 signature.
**OTel-GenAI** — OpenTelemetry GenAI semantic conventions. Still "Development" status as of Q1 2026; gated by `OTEL_SEMCONV_STABILITY_OPT_IN=gen_ai_latest_experimental`.
**P0a / P0a.5 / P0a.6** — shipped phases of mur-agent-runtime: per-agent runtime, identity + Noise + commander integration, identity rotation.
**PromptPatch / MessagePatch** — value types returned by mutate hooks; runtime folds them deterministically.
**Spotlighting** — wrap untrusted content in delimited tags + system-prompt instruction to never follow embedded directives. Microsoft Research, productized 2024.

References used during this brainstorming:
- OWASP Top 10 for LLM Applications (2025 ed., GenAI Security Project) + "Agentic AI – Threats & Mitigations" companion (Feb 2025).
- MITRE ATLAS v4.7.0 (2025).
- NIST AI 600-1 GenAI Profile (Jul 2024).
- Anthropic Constitutional Classifiers (Sharma et al. 2025), Sleeper Agents (Hubinger 2024), Alignment Faking (Greenblatt 2024).
- AgentDojo (ETHZ Spy Lab); InjecAgent (UIUC); HarmBench / JailbreakBench.
- OTel-GenAI semconv (`https://opentelemetry.io/docs/specs/semconv/gen-ai/`), MCP semconv.
- Tauri 2 documentation; `tauri-plugin-deep-link` v2; `tauri-plugin-global-shortcut` v2; `tauri-plugin-notification` v2; `tauri-plugin-clipboard-manager` v2.
- Character Card V3 spec (chara_card_v3); SillyTavern, Risu, Backyard, Chub schemas.
- Apple PrivacyInfo.xcprivacy + Required Reason API documentation.
- Hines et al. "Spotlighting" (arXiv:2403.14720); Bagdasaryan et al. "Abusing Images and Sounds" (arXiv:2307.10490); Greshake et al. (arXiv:2302.12173); Bailey et al. "Image Hijacks" (arXiv:2309.00236).
- Phylum `birdcage` 0.9; `landlock` crate; `seccompiler`; `wasmtime` 26.
- Kokoro 82M (HuggingFace user `hexgrad`, MIT); whisper.cpp; `sherpa-onnx`; `teloxide` 0.13; `whisper-rs`; `pdfium-render`; `image-rs`; `kamadak-exif`.
