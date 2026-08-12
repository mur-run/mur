# murmur TUI visibility fixes — design

Date: 2026-08-12
Status: approved (design), not yet implemented

Three reported defects in the `murmur` / `mur agent cli` TUI. All three are the
same class of bug: **something the operator must read is rendered into a region
too small to hold it, and the overflow is silently dropped.** Nothing warns; the
UI looks complete.

---

## 1. Approval panel loses its key row

### Symptom

The `approve tool call` modal shows the prompt, the tool name, and the JSON
input — but not the `[y] approve / [a] … / [n] deny` row. The operator sees a
blocking gate with no visible way to answer it.

### Cause

`mur-core/src/cmd/agent/cli/ui.rs::render_hitl` (line 1226) builds one `Vec<Line>`:

```
prompt · blank · tool: <name> · <up to 12 JSON lines> · blank · keys
```

and paints it as a single `Paragraph` with `Wrap { trim: false }` into
`centered_rect(70, 50, f.area())` — a box 50% of terminal height, 70% of width.
`Paragraph` clips; it does not scroll. Each JSON line soft-wraps at 70% width,
so a five-line pretty-printed input can occupy ten physical rows. On a 24-row
terminal the box has ~10 content rows, the JSON consumes all of them, and the
keys row — being last — falls off the bottom.

### Fix

The key row is the only part of this modal that is never optional. Pin it.

- Split the modal's inner area with `Layout::vertical([Min(0), Length(k)])`
  where `k` is the wrapped height of the key row at the modal's inner width
  (1 or 2 rows).
- Render the key row into the bottom chunk. It is now width-independent of the
  body and cannot be pushed out.
- Render the body into the top chunk. Keep `Wrap` for the prompt (it is prose).
  Render each JSON line **truncated to the inner width** rather than wrapped, so
  one input line costs exactly one row and the body's height is predictable.
- If the body still exceeds its chunk, drop the tail and append a dim
  `… N more lines` row as the last body row. Truncation the operator can see is
  not a bug; truncation they cannot see is.

The existing 12-line cap on JSON lines stays.

### Verification

- Unit: a pure `hitl_body_lines(hitl, inner_width, rows) -> Vec<Line>` returning
  at most `rows` lines, with the overflow notice present when it truncates.
- Render: paint `render_hitl` into a `Buffer` at 80×24 with a deliberately huge
  `tool_input`, then assert the string `approve` appears in the buffer's bottom
  rows. This is the test that would have caught the reported bug; a test that
  only inspects `lines` would not, because the key row *was* in `lines`.

---

## 2. Settlement block is cut off and ugly

### Symptom

The settlement reads

```
✘ parallel_jobs · Error: parallel_jobs failed: target 'repomanager' not authorized for parallel_j…
    {"jobs":[{"agent":"repomanager","description":"In repo /Volumes/Firecud…
```

— the useful half of two different strings is gone. Visually it is a bare grey
code fence with a fixed-length rule that does not match the pane.

### Cause

Two independent problems, in two crates.

**Truncation happens in the runtime, blind to the terminal.**
`mur-agent-runtime/src/turn_ledger.rs` truncates at fixed character budgets
before the text ever reaches a renderer: `truncate(raw, 72)` (line 218),
`truncate(s, 80)` (line 245), `truncate(detail, 120)` (lines 262, 265). At a
120-column terminal these throw away characters that would have fit; at a
60-column terminal they still overflow. The header is a hard-coded 41-character
rule (line 285).

**Presentation is a markdown code fence.** `render_settlement` emits a ```` ``` ````
block, so the TUI paints it with generic code-block styling: no colour on the
outcome glyphs, no visual separation from the reply above it.

### Fix

Split the concerns: the runtime keeps producing text (so `mur agent send`,
`--plain`, logs, and the Hub all still read it), and the TUI upgrades that text
to a card when it can.

**Runtime (`turn_ledger.rs`)**

- Remove the 72 / 80 / 120 caps. Keep one generous backstop — `truncate(_, 400)`
  — purely so a runaway error dump cannot flood the reply. The renderer, which
  knows the width, decides what fits.
- Replace the fixed 41-`─` rule with a plain `─ settlement ─` marker line. Width
  is not the runtime's business.
- Keep the fence. It is the interop contract for every non-TUI consumer.

**TUI**

- When an agent message is finalized (`app.rs`), detect a fenced block whose
  first content line starts with `─ settlement` and move it off `ChatMsg.text`
  into a new `ChatMsg.settlement: Option<Settlement>` — a parsed list of
  `{ outcome: Verified | Failed | Note, label: String, detail: String }`.
- `ui.rs` renders that struct **at paint time, where the pane width is known**:
  - A full-width background one shade off the transcript (new
    `Theme::card_bg`, defined for `DARK` / `LIGHT` / `MUR`).
  - A reverse-video `SETTLEMENT` title row.
  - A glyph column: `✔` in `theme.success`, `✘` in `theme.error`.
  - Detail text wrapped to the pane width with a hanging indent under the label,
    never truncated.
- If parsing fails, fall back to rendering the raw fence exactly as today. An
  unrecognized settlement must still be readable.

Because the card is rendered from the parsed struct at paint time, it reflows on
resize for free, and the same struct is what a future Hub GUI surface would
consume.

### Verification

- Runtime: existing `turn_ledger` tests assert full strings survive; add one
  asserting a 300-character error detail is not truncated.
- Parser: fence text → `Settlement` rows, including the
  `✔ verified (nothing ran — no evidence this works)` empty case.
- Render: paint the card at width 100 and at width 40; assert no `…` appears at
  either width and that the long detail occupies more rows at 40 than at 100.

---

## 3. The suggested-reply chooser hides the reply that prompted it

### Symptom

The agent finishes a reply, the numbered `1-9 pick` chooser opens, and most of
the reply vanishes. The operator is asked to choose between options whose
rationale they never got to read.

### Cause

Not a scroll — a clip. `chooser_band_height` (ui.rs:253) lets the chooser take
every row except `MIN_TRANSCRIPT_ROWS = 3`. The reply is a *finished* message,
but it is still **live** (unflushed), because the flush loop at ui.rs:715 has a
deliberate guard:

```rust
while end < settled && total > cap && total - u32::from(rows[end - start]) >= cap {
```

The third clause refuses to flush a message whose departure would leave the band
shorter than its capacity — a good rule when the band is the main surface,
because it prevents a 30-row reply from emptying the screen. But when the
chooser is open the band is 3 rows and is not the main surface at all, so the
guard traps the whole reply inside 3 tail-following rows. It never reaches
native scrollback, so the mouse wheel cannot recover it. Only `PageUp` reaches
it, which is not discoverable.

### Fix

When the chooser band is open, flush every finished message to scrollback:

- In the flush planner, when `app.completion` is `Some(state)` with
  `state.spaced && !state.items.is_empty()`, set `end = settled` — skip the
  short-band guard entirely.
- Rationale, to be written into the code as a comment: the guard exists to stop
  the transcript from emptying. With the chooser occupying the band there is
  nothing to keep full, and native scrollback is strictly better than a 3-row
  tail — it scrolls with the mouse and can be selected and copied.

`MIN_TRANSCRIPT_ROWS` stays at 3. The 3 remaining rows now show the tail of a
reply whose full text is one wheel-scroll above, which is the correct behaviour.

### Verification

- Unit on the flush planner: build an `App` with one finished 30-row agent
  message and a band cap of 3; assert `end == settled` when a spaced chooser is
  open, and `end == start` (unchanged) when it is not. The negative case is the
  point — it proves the fix is scoped to the chooser and did not just disable
  the guard.

---

## Scope

Two crates, three call sites, no format or protocol change:

- `mur-agent-runtime/src/turn_ledger.rs` — truncation caps, header rule.
- `mur-core/src/cmd/agent/cli/ui.rs` — HITL layout split, settlement card
  rendering, flush planner guard.
- `mur-core/src/cmd/agent/cli/app.rs` — `ChatMsg.settlement` extraction.
- `mur-core/src/cmd/agent/cli/theme.rs` — one new colour per skin.

Explicitly not in scope: structured settlement events over A2A (the fenced text
stays the wire format), Hub GUI settlement rendering, and any change to
`MIN_TRANSCRIPT_ROWS` or the chooser's own sizing.

## Deployment note

`turn_ledger.rs` lives in `mur-agent-runtime`. Running agents load their binary
once — fixing the truncation requires the agents to be restarted against the new
runtime, not just a `mur` rebuild.
