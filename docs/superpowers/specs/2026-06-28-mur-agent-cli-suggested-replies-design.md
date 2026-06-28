# `mur agent cli` agent-suggested replies (Type 2: `suggest_replies` tool)

**Date:** 2026-06-28
**Status:** Design approved (mechanism + UI), pending spec review
**Scope:** Type 2 of the `mur agent cli` autocomplete work — agent-driven reply
suggestions. The agent calls a `suggest_replies` tool with 1–5 short options;
the TUI renders them (ghost text for one, a chooser overlay for many) and `Tab`
fills the chosen text into the input. Builds on Type 1 (shipped, PR #532),
reusing its completion overlay.

## Goal

Let an agent offer the user quick-reply options at the end of a turn — "what
the LLM wants the user to input or choose" — surfaced as a `Tab`-to-fill
suggestion in the chat input, mirroring Claude Code's prompt suggestions but
**driven by the agent**.

## Decision: a structured `suggest_replies` tool (not text parsing)

Model→UI affordances should travel a **structured tool channel**, not free
text. This follows Anthropic's own agent-design guidance (promote an action to
a tool when you need to *render* it — Claude Code promotes question-asking to a
tool for exactly this; the Fable 5 `send_to_user` pattern gives a client-side
tool whose input the UI renders verbatim, since tool inputs are never
summarized). Rejected alternatives:

- **Fenced ```suggest block parsed by the TUI** — TUI-only and lazy, but relies
  on free-text format adherence (fragile), pollutes the visible reply, and is
  the anti-pattern the guidance warns against.
- **A2A reply `suggestions` envelope field** — cleanest data model, but doesn't
  solve *how the model produces* the suggestions (still needs a tool or
  structured output), so it's strictly more plumbing than the tool.

The tool reuses MUR's existing tool-call streaming (`StepStarted`), needs no
A2A envelope change, and a prescriptive tool description drives reliable
triggering (recent Opus models reach for tools conservatively — stating *when*
to call gives measurable lift).

## UX

- The agent calls `suggest_replies({replies: [...]})` (1–5 short strings, each a
  complete message the user could send) during its turn.
- Suggestions are revealed **after the turn finishes** (on `Done`), and only
  when the **input is empty** (don't clobber text the user is typing).
- **One suggestion → ghost text:** rendered as greyed placeholder in the input
  box. `Tab` (input empty) inserts it; the user then edits or presses `Enter`.
- **Two or more → chooser overlay:** the Type-1 completion overlay, populated
  with the suggestions. `↑↓`/`Ctrl+P`/`Ctrl+N` move, `Tab`/`Enter` accept
  (inserts the text), `Esc` dismisses.
- **Dismissal:** typing any character, submitting, or the start of a new turn
  clears the suggestion (ghost placeholder restored to `Type a message…`).

```
agent: Want me to open a PR or just push the branch?
┌─ message ──────────────────────────────────────────────┐
│ open a PR for this branch                              │   ← ghost (1 reply)
└─────────────────────────────────────────────────────────┘

           ─ or, for multiple ─

┌─ message ──────────────────────────────────────────────┐
│ ▏                                                       │
└─────────────────────────────────────────────────────────┘
 ╭ ↑↓ move · Tab accept · Esc close ──────────────────────╮
 │ open a PR for this branch                              │
 │ just push the branch                                   │
 │ show me the diff first                                 │
 ╰─────────────────────────────────────────────────────────╯
```

## Architecture

### 1. Runtime tool (`mur-agent-runtime`)

A built-in, no-side-effect tool.

- **Definition** (`ToolDef`, `llm/mod.rs`):
  - `name`: `"suggest_replies"`
  - `description` (prescriptive): *"Offer the user 1–5 short quick-reply options
    when they would likely pick from a small set — e.g. after you ask a question
    or propose a choice. Each option is a complete message the user could send
    verbatim. The options are shown as Tab-to-fill suggestions in the user's
    input; they do not end your turn. Do not call this for open-ended turns
    where there is no natural shortlist."*
  - `input_schema`: `{ type: object, additionalProperties: false,
    properties: { replies: { type: array, items: {type: string},
    minItems: 1, maxItems: 5 } }, required: ["replies"] }`
- **Executor** (`ToolExecutor`, `tools/mod.rs`): no-op. `execute` returns
  `Ok("ok")` (or `Ok("suggestions shown")`). The value reaches the model as the
  tool result and lets it finish its turn; the *user-facing* effect is carried
  by the streamed tool-call args, not the result.
- **Registration** (`build_tools`, `tools/registry.rs`): included **only for
  interactive streaming sessions**, so non-interactive callers (`mur agent
  send`, fleet runs, mobile) never get it and the model can't waste a call.
  Gate: the runtime already routes a per-task `step_notifier` to a streaming
  client (`task_runner.rs:1079`); register `suggest_replies` iff such an
  interactive client capability is present. (If a clean request-time flag is
  cheaper than threading interactivity into `build_tools`, the cli sets a
  capability on dial and the runtime gates on that — the plan picks whichever
  is the smaller change; both produce the same observable behavior.)
- **HITL policy** (`resolve_tool_policy`, `task_runner.rs:1088`): treated as
  `Allow` (auto-approved, never pauses) — it has no side effects. A built-in
  exemption so it works regardless of the agent's default `Ask` policy.

### 2. TUI interception (`mur-core/src/cmd/agent/cli/`)

The tool-call args already flow to the TUI as
`StreamMsg::StepStarted { name, args }` (`stream.rs:251` → `handle_stream`,
`mod.rs:1036`).

- **New App state** (`app.rs`):
  - `pending_suggestions: Vec<String>` — captured mid-turn, revealed on `Done`.
  - `suggestion_ghost: Option<String>` — the single-reply ghost currently shown.
- **`handle_stream` changes** (`mod.rs`):
  - `StepStarted { name, args, .. }` where `name == "suggest_replies"`: do **not**
    `push_step_started` (no step card). Parse `args` → set
    `app.pending_suggestions`. (Returns early so the suggestion tool-call is
    invisible in the transcript.)
  - `Done { .. }`: if `pending_suggestions` is non-empty, the input is empty, and
    not streaming → **reveal**, then clear `pending_suggestions`:
    - 1 item → `app.suggestion_ghost = Some(text)`; `app.input.set_placeholder_text(text)`.
    - ≥2 items → populate `app.completion` (the Type-1 `CompletionState`) with
      one `Candidate { display: s, insert: s, has_children: false }` per reply.
- **Key handling** (`mod.rs`), priority order:
  1. `app.completion.is_some()` → existing Type-1 menu keys (unchanged) —
     accept inserts the suggestion, `Esc` closes. **No new code.**
  2. else `app.suggestion_ghost.is_some()` and input empty and `Tab` → insert the
     ghost into the input, clear ghost + restore default placeholder.
  3. else `Tab` → existing Type-1 `refresh_completion` (open the slash menu).
- **Ghost clearing**: any input edit, `submit`, and the start of a new turn clear
  `suggestion_ghost` and restore the `Type a message…` placeholder.

### Pure helpers (testable without a TUI)

- `parse_suggestions(args: &Value) -> Vec<String>` — extract `replies` (string
  array), drop empties, cap at 5. Returns empty on any malformed shape
  (fail-soft).

## Data flow

```
model calls suggest_replies(tool)
  → runtime: ToolPolicy::Allow → no-op execute → Ok("ok")  [turn continues]
  → runtime emits step/started {name:"suggest_replies", args:{replies}}
  → a2a_dial → StreamMsg::StepStarted{name, args}
  → handle_stream: stash app.pending_suggestions  (no step card)
  → StreamMsg::Done: reveal (ghost if 1, completion overlay if ≥2)
  → user presses Tab → text inserted into input
  → Enter → ordinary user message sent to the agent
```

## Error handling / compatibility

- **Old runtime, new TUI:** tool not offered → no `suggest_replies` calls → no
  suggestions. Harmless.
- **New runtime, old TUI:** the tool call streams as an ordinary `StepStarted`
  and renders as a normal step card (`suggest_replies(...)`). Slightly ugly but
  harmless; resolved once the TUI ships.
- **Malformed args:** `parse_suggestions` returns empty → nothing revealed.
- **Non-interactive sessions:** tool not registered → model can't call it.
- **Reveal guard:** never overwrite a non-empty input; suggestions only appear
  on an empty composer after the turn ends.

## Out of scope

- Persisting suggestions across turns / a "latch" so `Esc` stays dismissed (Type
  1's deferred `dismissed` flag would cover both).
- Suggestions on mobile / Hub surfaces (this spec is the `mur agent cli` TUI).
- Auto-generating suggestions when the agent didn't ask for them (no extra model
  call — emit-driven only, to stay cheap and quiet).

## Testing

- **Runtime** (`mur-agent-runtime`):
  - `suggest_replies` executor returns `Ok` with no side effects (unit).
  - `resolve_tool_policy("suggest_replies")` → `Allow` (unit).
  - `build_tools` includes `suggest_replies` for an interactive session and
    omits it for a non-interactive one (unit).
- **TUI** (`mur-core`):
  - `parse_suggestions` — valid array, empties dropped, >5 capped, malformed →
    empty (unit, pure).
  - reveal selection: 1 → ghost field set; ≥2 → `completion` populated with the
    right candidates; empty / non-empty-input → nothing revealed (unit on a test
    `App`).
  - ghost `Tab`-fill inserts the text and clears the ghost (unit).
- **Smoke (operator):** a live agent calls `suggest_replies`; the TUI shows the
  ghost / chooser; `Tab` fills; `Enter` sends. (Headless agents can't drive a
  TUI — operator-verified, as with Type 1.)
