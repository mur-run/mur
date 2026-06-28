# `mur agent cli` — Glass Box (Step Transparency) — Design Spec

- **Date:** 2026-06-27
- **Status:** Draft (design); pending user review → writing-plans
- **Author:** David Chang (with Claude Code, ultracode research)
- **Scope:** `mur-agent-runtime` (step-event emission) + `mur-core/src/cmd/agent/cli/` (render). No GUI.
- **Research basis:** Workflow `wf_a523621f-e2b` (6 web-research agents on 2026 terminal-AI-agent UX + deep-read + synthesis), a codebase map of the current cli TUI, and a verified brief on Claude Code long-session handling. Competitive baseline: Claude Code, Codex CLI, Gemini CLI, Aider, OpenCode/Crush, Warp.
- **Supersedes nothing.** Orthogonal to `2026-06-17-agent-cli-ux-improvements-design.md` (keyboard/skin polish) and `2026-06-24-agent-runtime-upgrade-restart-design.md` (whose proto-version gate this reuses).

## Thesis

> The user must always know exactly what the LLM is doing right now — streaming text, full reasoning, which tool with which args, what it returned, what it decided, what it cost, how long it took, and where it's blocked.

The current cli is **blind to auto-approved tool steps.** The runtime streams only `message/delta` (text + `thinking` flag) + `tool/approval_needed` (HITL) + a final task JSON the cli mostly discards. Tools that don't need approval are never surfaced as steps — they're buried in the final JSON, of which the cli keeps only the last text block.

So step-transparency is **not a frontend job.** The load-bearing change is a runtime **step-event stream** (Codex's `started → … → completed` lifecycle, minimized). The tool cards, footer, and controls all hang off that spine. Everything the cli can already see (streamed text, reasoning deltas, HITL, `Task.usage`) is reused — we stop *throwing it away*, we don't re-plumb it.

## North star → concrete goals

1. **Every tool call is a visible step** — name + args + result + error + duration — live, for *all* tools (not just HITL-gated ones).
2. **Reasoning is never erased.** Today it streams then vanishes on turn-finish; keep it.
3. **A persistent footer** answering "what state, how many tokens, what cost, how full is the window, how long" at a glance.
4. **Inline progressive disclosure** (Hybrid model, not a separate pane): summary by default, one key to expand args / result / diff / reasoning.
5. **Control:** single-key interrupt with a live timer, inline risk-tiered approvals, mid-turn steering, scrollback escape hatch.
6. **Graceful degradation** against older runtimes (the known stale-binary drift) — no hard break.

## Non-goals (explicit YAGNI — do NOT build)

- **Context compaction / auto-compact / `/compact` / `/clear` / `/rewind`.** Real and valuable, but a *different feature area* (context management, not step-transparency). Sibling spec — see [Related work](#related-work--sibling-specs). This spec only **reserves a footer hook** so it composes later.
- **A separate "mission control" multi-pane layout.** Rejected during brainstorm in favor of the inline Hybrid model. The inline step stream *is* the timeline.
- **A new persistent store for full tool output.** Full output for expansion comes from the final task JSON (already complete) + the existing compress/`mur_retrieve` path. No new on-disk object.
- **A new A2A transport or method family.** Reuse the existing JSON-RPC streaming socket; add notifications, not a protocol.
- **Reasoning-summarization model** (Claude's `display:"summarized"` second-model trick). MUR shows raw reasoning as-is; summarization is out of scope.
- **Per-provider rate-limit fabrication.** Render quota meters *only* when the provider actually surfaces quota; otherwise omit the field. No invented numbers.

## Decisions (locked)

| ID | Decision |
|----|----------|
| **D0** | The spine is a **runtime change**: two new JSON-RPC notifications (`step/started`, `step/completed`) emitted at the tool call-site in `mur-agent-runtime`. Non-negotiable — the requirement is unreachable frontend-only. |
| **D1** | **Reasoning protocol is unchanged.** Reuse the existing `message/delta` `thinking:true` stream; the only change is `App` stops clearing `thinking` on `finish_agent_turn`. No `step/started` for reasoning. |
| **D2** | **Full-output expansion reads from the final task JSON**, mapped `step_id → tool_result`. Inline `step/completed` carries only a truncated `output` + `full_len`. No new storage in v1. Oversized output during a *running* turn stays truncated until completion. |
| **D3** | **Backward-compat via the existing proto-version gate** (`2026-06-24-agent-runtime-upgrade-restart`). New cli + old runtime → no step events arrive → fall back to today's text+HITL rendering + a one-line "restart this agent for step view" hint. Never a hard error. |
| **D4** | **HITL moves inline onto the tool card** (state `AwaitingApproval`), replacing the centered modal. Keys `[y]/[a]/[n]` unchanged; approval still rides the existing separate responder connection. |
| **D5** | **Reasoning default = always fully visible** (user choice), with a `/reasoning [full\|collapsed]` toggle and auto-collapse under `--plain`/screen-reader mode. The toggle is a knob, not a re-litigation. |
| **D6** | **Footer = full statusline parity.** Cost is from `models.yaml` rates, labeled `est`, `—` when the model has no price. Context bar is **input-only** (`input + cache_creation + cache_read`) over the model's window, 3-threshold (green<70 / yellow / red≥90). Rate-limit field renders only when provider quota is available. |
| **D7** | **Compaction is a sibling spec.** This spec reserves exactly one integration point: when `ctx ≥ threshold` the footer shows a `● /compact` affordance (no-op stub until the context-management spec lands). |
| **D8** | **Phased delivery.** P1 (spine + cards + footer), P2 (control + diffs), P3 (power extras). Each phase is its own plan → PR, independently shippable. P1 alone delivers the north star. |

## A. Runtime step-event protocol (the spine)

Two notifications on the existing streaming socket (`~/.mur/agents/<name>/running.lock` JSON-RPC), emitted around the tool call-site (the same site that already emits `tool/approval_needed`):

```jsonc
// server → client, during a turn
{ "method": "step/started",   "params": {
    "step_id": "s-1", "kind": "tool", "name": "edit", "args": { ... }, "ts": 1782... } }

{ "method": "step/completed", "params": {
    "step_id": "s-1", "ok": true,
    "output": "…first N lines…", "truncated": true, "full_len": 412,
    "error": null, "duration_ms": 86, "ts": 1782... } }
```

- **Lifecycle.** Plain tool: `started → completed`. HITL-gated tool: `started → tool/approval_needed → completed` (the card shows running → awaiting approval → done). Denied: `started → completed{ok:false, error:"denied"}`.
- **Reasoning** (D1): no step event — keep `message/delta{thinking:true}`.
- **Usage.** Read `input_tokens / output_tokens / cache_creation_input_tokens / cache_read_input_tokens` from the final task (`Task.usage`, already flowing — proven by the fleet budget work). During streaming, show a live *estimate* (`output ≈ chars/4`) and reconcile to the accurate value at `Done`. An optional incremental `turn/usage` notification is a P3 nicety, not required.

### `dial_message_streaming` signature change

Current (`a2a_dial` / `stream.rs`):

```rust
pub fn dial_message_streaming(
    home: &Path, agent_name: &str, params: Value,
    on_delta: impl FnMut(&str, bool, &str),   // (text, thinking, task_id)
    on_hitl:  impl FnMut(Value),
) -> Result<Value>
```

Add one callback, mirroring the existing pattern (smallest diff that fits house style):

```rust
    on_step: impl FnMut(StepEvent),            // StepEvent::{Started, Completed}
```

`stream.rs` parses `step/started` / `step/completed` into `StepEvent` and forwards new `StreamMsg` variants to the cli's mpsc channel.

### `StreamMsg` additions (`stream.rs`)

Existing: `Delta`, `Hitl`, `Done`, `Err`, `Note`, `ShellDone`. Add:

```rust
StepStarted   { task_id: String, step_id: String, name: String, args: Value },
StepCompleted { task_id: String, step_id: String, ok: bool,
                output: String, truncated: bool, full_len: usize,
                error: Option<String>, duration_ms: u64 },
```

## B. cli render model — inline step stream

The transcript becomes a list of typed, foldable nodes (replacing flat text accumulation), rendered **inline** (Hybrid, no pane):

```rust
enum Node {
    User(String),
    Reasoning(String),         // D5: expanded by default
    Assistant(ChatMsg),        // streamed markdown (existing ChatMsg reused)
    Tool(StepNode),
    System(String),            // errors, notes, retry/wait banners
}

struct StepNode {
    id: String, name: String, args: Value,
    state: StepState,          // Running | AwaitingApproval | Done | Error
    output: String, full_len: usize, error: Option<String>,
    started: Instant, duration: Option<Duration>,
    expanded: bool, is_edit: bool,   // is_edit → render diff body
}
```

**Tool card.** Header: `{glyph} {name} {arg-summary} · {state} · {elapsed}`. Body (expandable): pretty-printed full args + result (`… +N lines`, expand pulls full from task JSON) + red error. Edit tools render a diff body (§E).

```
╭ 🔧 edit  src/auth.rs · running · 0m08s ──────────╮   ← collapsed: header only
│ approve?  [y]es  [a]lways  [n]o                  │   ← AwaitingApproval: inline
╰──────────────────────────────────────────────────╯
✔ read  src/auth.rs · 412 lines · 8ms          [tab] ← Done, collapsed
✗ bash  cargo test · exit 101 · 2.3s           [tab] ← Error (red)
```

**Append-only stability** (the screen-reader / redraw-churn fix the research flagged): only the *running* node mutates; completed nodes are stable blocks. Apply diff-before-repaint in the normal renderer too.

**Keys.** `↑/↓` move a selection cursor across step cards (and scroll); `tab`/`enter` expand/collapse the focused card; `y/a/n` inline HITL; `r` retry recently-denied; `esc` interrupt (double-esc semantics from the 0617 spec preserved); `Ctrl+O` scrollback dump (§D); typing + `enter` while running = mid-turn steer (§D).

## C. Footer — full statusline parity (D6)

Persistent row above the input box:

```
{◇/◐/✋ state} · {turn}/{sess} tok · ${cost} est · ctx ▓▓▓░░░ 32% · {wall}/{api} · {5h 41%} · esc=stop
```

- **state glyph:** Ready `◇` / Working `◐`(spinner) / Action-Required `✋`.
- **context %** = `(input + cache_creation + cache_read) / window` (input-only), window from `models.yaml`; bar color green<70 / yellow<90 / red≥90.
- **cost** = `input·in_rate + output·out_rate` from `models.yaml`; `—` when unpriced (local/proxy). Always labeled `est`.
- **dual clock:** `wall = now − turn_start`; `api ≈ Σ step durations + model stream time`.
- **rate-limit:** rendered only if the provider surfaces quota (e.g. Anthropic headers via cc-proxy); omitted otherwise.
- **D7 hook:** at `ctx ≥ threshold`, append a `● /compact` affordance (stub).

`footer.rs` owns this math; unit-tested in isolation.

## D. Control & power extras

| Feature | Behavior | Notes |
|---------|----------|-------|
| **Interrupt + timer** | `esc` cancels in-flight; running card shows `· 0m12s · esc=stop`. | Reuses existing cancel path. |
| **Mid-turn steering** | Type + `enter` while running → inject guidance without killing the turn (`turn/steer` runtime method appends a user message to in-flight context). | Proto-gated; degrades to "queued, sent next turn" on old runtimes. |
| **Risk-tiered approvals** | Reuse mur's SHA-256-pinned HITL gate: reads/searches auto-execute + log; write/destructive gate inline on the card. Recently-denied list + `r` to retry. | No new gate; new *surfacing* of the existing one. |
| **Scrollback escape hatch** | `Ctrl+O` dumps the full expanded transcript to native scrollback / `$EDITOR` so `Cmd+f` / tmux copy works. | Live view stays virtualized. |
| **Notify-on-blur** | Desktop notification only when the terminal is unfocused (OpenCode + Crush converged default). | Reuses companion/notify if present; else OS bell. |
| **Active budget** | `--budget-usd X` / `--budget-tokens N`: footer shows remaining; turn aborts when exhausted. | Reuses fleet's real per-token accounting. Off by default. |
| **Plain / screen-reader mode** | `--plain` (or auto-detect): finalized append-only blocks, no spinner/timer churn, reasoning auto-collapsed. | Fixes the line-mutation announcement bug. |
| **Retry / wait banners** | Non-failure `System` node: "waiting for API… will retry · attempt x/y" after stream silence / on backoff. | So a stall never reads as a hang. |

## E. Diff viewer

Edit-tool cards render a syntax-highlighted unified diff as the body (the diff *is* the approval surface). Built from the edit tool's `old`/`new` args — no extra runtime data. `/diff` opens a per-turn navigable view (Current vs prior turns) with per-file `+/-` stats and state tags (`new`/`deleted`/`binary`/`truncated`). Viewport-responsive (inline unified default; stacked when narrow).

## Module layout (ponytail: split, don't bloat)

`mod.rs` is already **857 lines** (over the 800-line rule) and `ui.rs` is 345. New code lands as:

- `cli/step.rs` — `Node` / `StepNode` model + state machine (started/delta/completed, expand toggles, truncation). **Unit-tested.**
- `cli/footer.rs` — token/cost/context/timer math. **Unit-tested.**
- `cli/budget.rs` — active-budget guard (reuse fleet accounting).
- `cli/render/{transcript,card,footer,diff}.rs` — split out of `ui.rs` as it crosses 800.
- `stream.rs` — `StepEvent`, new `StreamMsg` variants, notification parsing.
- `mur-agent-runtime` — emit `step/started` / `step/completed` at the tool call-site; `turn/steer` method (P3).

`mod.rs` itself only gains arms for the new `StreamMsg` variants; the logic lives in the new modules.

## Backward-compat & proto-gating

Reuse `2026-06-24-agent-runtime-upgrade-restart`'s proto-version gate. If the dialed runtime predates step events, the cli receives no `step/*` notifications → renders today's text + HITL behavior, and shows a single dismissible `System` hint: `↻ restart this agent (mur agent restart <name>) to see per-step detail`. Never auto-restart (the memory'd needrestart-style rule).

## Phasing

| Phase | Contents | Delivers |
|-------|----------|----------|
| **P1 — Spine** | runtime `step/started`+`step/completed`; `StreamMsg` variants; `cli/step.rs` cards (name/args/result/error, truncate+expand); reasoning-kept (D1); `cli/footer.rs` (tokens/cost/context/timers); `esc`+timer; proto-degrade (D3). | The north star — "know each step" — standalone. |
| **P2 — Control & diffs** | inline risk-tiered HITL on cards (D4) + recently-denied/`r`; `/diff` per-turn diff viewer (§E); `Ctrl+O` scrollback hatch; notify-on-blur. | Rounds out the 2026 baseline. |
| **P3 — Power extras** | `turn/steer` mid-turn steering; `--budget-*` active budget; `--plain`/screen-reader mode; retry/wait banners; optional incremental `turn/usage`. | Maximal transparency + control. |

## Testing

- **Runtime:** a tool call emits `step/started`+`step/completed` with correct args/output/`duration_ms`; an HITL-gated tool emits `started → approval → completed`; a denied tool emits `completed{ok:false}`.
- **cli `step.rs`:** state machine (started→completed→expand toggle), truncation + `full_len`, full-expand maps to task-JSON result.
- **cli `footer.rs`:** context % is input-only; threshold colors at 69/70/89/90; cost `—` when unpriced; dual-clock split.
- **Proto-degrade:** a stream with zero `step/*` events renders text + the upgrade hint, no panic.
- One runnable `assert`-based check per non-trivial unit; no framework, no fixtures (ponytail).

## Related work & sibling specs

- **Context management (compaction) — future sibling spec.** Claude Code's long-session model is auto-compact at ~83.5% (`CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`, ~33K reserved buffer) + manual `/compact [focus]` + `/clear` + `/rewind` (warm-cache truncate) + `/context` itemization + durable `CLAUDE.md`/memory + layered prompt caching + session resume. MUR is well-positioned: the **channel** already persists every turn (signed, resumable), tool outputs already **compress/`mur_retrieve`**, and delegate/fleet already isolate context like subagents. That spec wires those into a sense → compact → continue loop; this spec only reserves the footer `● /compact` hook (D7).
- `2026-06-17-agent-cli-ux-improvements-design.md` — keyboard/skin polish; double-esc semantics preserved here.
- `2026-06-24-agent-runtime-upgrade-restart-design.md` — proto-version gate reused for D3.

## Open risks

1. **Stale-runtime drift** (memory: `project_agent_runtime_upgrade_restart`) — mitigated by D3 proto-gate + upgrade hint.
2. **Cost accuracy** for local/proxy models — labeled `est`, `—` when unpriced; never presented as the real bill.
3. **Rate-limit data availability** — field is conditional, not fabricated.
4. **Always-visible reasoning noise** in long sessions — mitigated by `/reasoning collapsed` + `--plain` auto-collapse (D5).
5. **Dual-clock `api` time is approximate** (Σ step durations + stream time) — acceptable; labeled, not billed.
