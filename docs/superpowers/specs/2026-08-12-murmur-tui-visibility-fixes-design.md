# murmur TUI visibility fixes — design

Date: 2026-08-12
Status: implemented (verified 2026-08-17 against `main` @ 78736ae7)

All three sections have shipped. §1 landed with an extra `keys_inert` guard
(`cmd/agent/cli/ui.rs:1421-1444`) beyond what this design specified. §2's caps
are now `RUNAWAY_BACKSTOP` (`mur-agent-runtime/src/turn_ledger.rs:218,245,267,270`)
and the rule is `─ settlement ─` (`:290`), with `ChatMsg.settlement`
(`cmd/agent/cli/app.rs:91`), `settlement::split()`, and `theme.card_bg` for all
three skins (`cmd/agent/cli/theme.rs:53,78,103`). §3 is `chooser_band_height`
(`cmd/agent/cli/ui.rs:274-300`) and `scroll_marker` (`:859-866`).

One deliberate deviation: `ChatMsg.settlement` is `Option<String>` parsed at
render time by `settlement.rs`, not the `Option<Settlement>` struct this design
sketched. The rendering contract is unchanged.

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

The text is not lost, and this is not a flush bug. The reply stays live in the
band by design — the flush planner (ui.rs:680) deliberately measures capacity
with `chooser_h = 0` (ui.rs:463-473), because a flush is one-way and the chooser
is transient; flushing to fit it would leave a blank pane above the composer the
moment it closes. `render_transcript` tail-follows and `PageUp` still reaches
the text (ui.rs:861-863). All of that is correct and must not change.

The actual defect is a wrong constant, and a missing indicator.

`chooser_band_height` (ui.rs:253) reserves `MIN_TRANSCRIPT_ROWS = 3` rows for
the transcript and gives the chooser everything else. With the Inline viewport
fixed at `INLINE_VIEWPORT_HEIGHT = 20`:

```
available = 20 − (input 3 + status 1) − 3          = 13
full      = 3 items × (label + desc + spacer) + 2  = 11     ≤ 13, so it is used
transcript= 20 − 3 − 1 − 11 = 5 rows − 2 borders   = 3 content rows
```

Three rows. And `scroll_page` is set to the visible height, so `PageUp` pages
through a 20-line reply three lines at a time, through a three-line peephole,
with nothing on screen saying there is anything above. That is why it reads as
"it vanished".

The function already degrades gracefully — it has a one-line-per-item `compact`
form and a `Ctrl+↑/↓` manual resize. It was simply never given a floor worth
degrading toward.

### Fix

Set the floor correctly and let the existing fallback do the work.

**`chooser_band_height`** — reserve a readable transcript instead of 3 rows:

- `floor = max(MIN_TRANSCRIPT_ROWS, total_h * 40 / 100)`.
- Compute `available` against `floor`. If the result is at least `compact`, use
  it. If it is not — a short terminal — fall back to reserving
  `MIN_TRANSCRIPT_ROWS`, as today. The chooser must stay usable; it is the
  thing the operator has to act on.
- When `full` does not fit `available`, take exactly `compact` rather than all
  of `available`. Today the band is padded out to `available` even in compact
  form, which spends rows on nothing.
- `chooser_grow` (Ctrl+↑/↓) is unchanged and still expands back to the spaced
  form on demand.

At `total_h = 20` with three options this yields `floor = 8`, `available = 8`,
`full = 11 > 8` → compact = 5 rows for the chooser and **9 content rows** for
the transcript, three times today's, with option descriptions still present in
their one-line form.

**`render_transcript`** — never hide content silently. When
`max_scroll > 0`, add a right-aligned title on the transcript's top border:
`↑ N more · PgUp` when `scroll_back == 0`, and `↑ N · PgDn to follow` when the
operator has already scrolled. This is independent of the chooser and fixes the
same silent-hiding for every other cause (a long reply, a grown composer, the
fleet rail).

The flush planner is not touched.

### Verification

- Unit on `chooser_band_height` at `total_h = 20`, `input_height = 3`, three
  items with descriptions: assert the returned height is `compact` (5), not 11.
- Unit at `total_h = 12` (short terminal): assert the chooser still gets at
  least its compact height — the floor must yield rather than squeeze the
  chooser out.
- Unit asserting `chooser_grow = +6` still reaches the spaced height, so the
  escape hatch survives.
- Render: paint the transcript with 30 lines into a 5-row band and assert
  `↑ 25 more · PgUp` appears in the top border row; then with 3 lines into the
  same band and assert no such marker appears.

---

## Scope

Two crates, three call sites, no format or protocol change:

- `mur-agent-runtime/src/turn_ledger.rs` — truncation caps, header rule.
- `mur-core/src/cmd/agent/cli/ui.rs` — HITL layout split, settlement card
  rendering, chooser floor, transcript scroll indicator.
- `mur-core/src/cmd/agent/cli/app.rs` — `ChatMsg.settlement` extraction.
- `mur-core/src/cmd/agent/cli/theme.rs` — one new colour per skin.

Explicitly not in scope: structured settlement events over A2A (the fenced text
stays the wire format), Hub GUI settlement rendering, and the flush planner —
its chooser-blind capacity rule is deliberate and stays exactly as it is.

## Deployment note

`turn_ledger.rs` lives in `mur-agent-runtime`. Running agents load their binary
once — fixing the truncation requires the agents to be restarted against the new
runtime, not just a `mur` rebuild.
