# murmur TUI — composer geometry and tool lines — design

Date: 2026-08-17
Status: approved (design), not yet implemented

The 2026-08-12 visibility-fixes spec named the systemic defect precisely:

> something the operator must read is rendered into a region too small to hold
> it, and the overflow is silently dropped

That spec's three cases have since shipped (see *Prior spec status* below). The
three cases here are the same defect in the regions it did not cover — the
composer, and the tool-call line — plus one new variant: **one fact rendered by
two renderers that disagree.**

---

## 0. What exists today (verified against `main` @ 78736ae7)

| Fact | Location |
|---|---|
| Inline viewport, fixed 20 rows | `cmd/agent/cli/mod.rs:158` (`INLINE_VIEWPORT_HEIGHT`) |
| Composer height clamp 3..8 | `cmd/agent/cli/ui.rs:31-32`, applied at `:59` |
| Resize recovery re-anchors **at the top** | `cmd/agent/cli/mod.rs:558-591` (`rebuild_after_resize`) |
| Scroll indicator already exists | `cmd/agent/cli/ui.rs:859-866` (`scroll_marker`) |
| Chooser transcript floor already exists | `cmd/agent/cli/ui.rs:268-300` |
| Red/green diff renderer already exists | `cmd/agent/cli/diff.rs:42` (`edit_diff_lines`) |
| …but is wired on **one** path only | `cmd/agent/cli/render_card.rs:81` |
| `bash` tool schema has no title field | `mur-agent-runtime/src/tools/bash.rs:193-210` (`command`, `cwd` only) |

Three of these are load-bearing good news: the scroll indicator, the transcript
floor, and the diff renderer all exist and are correct. Two of the three fixes
below are therefore *wiring*, not new rendering.

---

## 1. The composer must sit at the bottom

**Root cause.** `rebuild_after_resize` recovers from a terminal reflow by wiping
screen and scrollback and re-anchoring a fresh viewport **at the top**
(`mod.rs:565`), then setting `flushed_upto = 0` so the transcript re-emits and
pushes the viewport down. With a short transcript there is nothing to push with,
so the composer stays near the top of a tall window with dead space below.

This is not a coding error. The module comment records that the obvious
alternatives were tried and rejected: an in-place fix leaves a stale copy of the
old viewport in scrollback (ratatui's own `autoresize` leaks identically), and
letting the viewport fill the screen sends `insert_before` down its degenerate
draw-over-the-top path, which garbles scrollback rows.

**Fix.** Keep the scorched-earth recovery exactly as it is. After re-anchoring
and re-emitting the transcript, count the rows that re-emission actually printed
into scrollback (the `insert_before` heights already summed by `flush_finished`,
not the transcript's line count before wrapping) and emit
`rows.saturating_sub(viewport_h + emitted_rows)` blank lines, so the viewport
lands at the bottom regardless of transcript length. When re-emission alone
already fills the screen the term saturates to zero and nothing is printed.

This touches neither rejected path: it does not modify the viewport in place,
and it does not change the viewport's height. It only supplies the scrollback
content that the existing design was already waiting for.

`rebuild_after_resize` must return with the composer at `rows - input_h - 1` for
any transcript length, including zero.

---

## 2. The composer may grow into the transcript — visibly

**Root cause.** `INPUT_H_MAX = 8` caps the composer at six content lines
(`ui.rs:31-32`, `:59`). Beyond that, typed text is silently scrolled out of view
and the operator must arrow through their own message. The cap exists because
the viewport is only 20 rows and the transcript band would otherwise vanish.

**Fix.** Raise the ceiling to `viewport_h - 1` (everything but the status line).
The transcript band may be squeezed to zero while the operator is typing, and
recovers when the message is sent.

**The squeeze must be visible.** Whenever the band is reduced below the rows it
wants, render `scroll_marker()` (`ui.rs:859-866`) — the `↑ N more · PgUp`
indicator that already exists. Truncation the operator can see is not a bug;
truncation they cannot see is. Growing the composer without the marker would
reintroduce the exact defect the 2026-08-12 spec removed elsewhere.

**The chooser's 40 % floor is unchanged** (`ui.rs:268-300`). These are different
situations and must not share a rule: the suggested-reply chooser needs
transcript context *in order to choose*, so the transcript wins. A composer full
of the operator's own in-progress text does not — they are looking at what they
are typing. Keep `TRANSCRIPT_FLOOR_PCT` applied to the chooser only.

---

## 3. Tool lines: one line, one shape

Today a tool call renders as its raw wire name and full argument string, middle-
truncated to fit:

```
mcp__media__mur_retrieve a4f158c092c9423c17601cb5
bash cat mur-hub-gui/ui/src/components/s…onents/chats/chatList.ts | head -40
edit_file /Volumes/…/channel.rs  → replaced 1 occurrence(s) in /Volumes/… · 1ms
```

**Target shape** — `<kind> <title>(<detail>)`, where `<detail>` is rendered in a
muted style:

```
mcp  <title>(media__mur_retrieve a4f158c092c9423c17601cb5)
mcp  <title>(media__mur_project_status)
bash <title>(cat mur-hub-gui/ui/src/components/s…onents/chats/chatList.ts | head -40)
```

The muted colour is a new token on `Skin` (`cmd/agent/cli/theme.rs`), defined for
all three skins alongside `card_bg`. No literal colours at call sites.

**`edit_file` is an explicit exception** and does not take the `<kind> <title>(…)`
shape. It renders as a header plus the change itself:

```
Update(/Volumes/Firecuda4tb/Projects/mur/mur-common/src/channel.rs)
  - removed lines on a red ground
  + added lines on a green ground
```

`edit_diff_lines()` (`diff.rs:42`) already produces exactly these red/green
lines. It is wired only into the card path (`render_card.rs:81`); the flat line
path renders `→ replaced 1 occurrence(s) in …` instead.

**That divergence is the defect, and collapsing it is the fix.** Do not add a
second diff renderer. Both paths must call `edit_diff_lines`; if a path cannot
show a diff at its size, it shows the header and a visible indicator, never a
different summary of the same edit.

---

## 4. Where the title comes from

`<title>` is a short natural-language label for what the call is doing. It does
not exist today and nothing generates it.

**MUR-native tools — the model supplies it.** Add an optional `description`
property to the input schema of every tool MUR itself defines — that is, every
`ToolExecutor` under `mur-agent-runtime/src/tools/` whose schema MUR authors
(`bash`, `read_file`, `write_file`, `edit_file`, `open_item`, `remember`,
`suggest`, `fleet_run`), and explicitly **not** `mcp.rs`, whose schema comes off
the wire. The model fills it as part of the call it was already making: no extra
model round trip, no added latency, no added cost. It stays optional — a call
without one falls back to the rule path below.

**MCP tools — derived by rule.** An MCP tool's input schema is supplied by its
server, not by MUR. MUR does not rewrite it. The title is derived from the tool
name and its salient argument (`media__mur_retrieve <hash>` → `retrieve`).

Injecting a MUR-owned property into third-party schemas and stripping it before
forwarding was considered and rejected: it mutates a contract MUR does not own,
can break servers that validate strictly, and touches the area
`docs/architecture/mcp-supply-chain.md` governs. The quality gain does not
justify it.

Rule-derived titles are worse than model-written ones. That is accepted: the
line is still shorter and better grouped than today's raw wire name, and the
full detail remains visible in the muted parenthetical.

---

## Scope

**In:** bottom-anchoring after resize; composer growth with a visible squeeze
indicator; the `<kind> <title>(<detail>)` tool line; `Update(path)` + diff for
`edit_file` on both render paths; the muted `Skin` token; the optional
`description` property on MUR-native tool schemas; rule-derived MCP titles.

**Out:** the agent home-directory hallucination (`/Users/i/`, `/Users/lidj/`) —
that is a runtime prompt defect, not a rendering one, and is handled as a
separate bounded fix; the run-status line in the TUI (belongs to the job/fleet
spec, and must land after it); anything already shipped per the section below.

**Ordering.** `2026-08-17-job-fleet-run-status-design.md` must be implemented
first. It changes the semantics of `hitl::gate::DEFAULT_TIMEOUT`, which this
TUI reads directly to draw its approval countdown (`ui.rs:1118-1123`,
`mod.rs:783`, `:1416`, `:2571`). Doing the TUI work first would mean rewriting
those call sites twice.

---

## Prior spec status — correction required

`docs/superpowers/specs/2026-08-12-murmur-tui-visibility-fixes-design.md` is
marked *"approved (design), not yet implemented"*. **That status is stale — all
three of its sections have shipped:**

- §1 approval key row — implemented, and extended past the spec with the
  `keys_inert` guard (`ui.rs:1421-1444`) so keys do not silently grant while the
  composer holds text.
- §2 settlement — the 72/80/120 caps are now `RUNAWAY_BACKSTOP`
  (`mur-agent-runtime/src/turn_ledger.rs:218,245,267,270`); the 41-character rule
  is now `─ settlement ─` (`:290`); `ChatMsg.settlement` (`app.rs:91`),
  `settlement::split()`, and `theme.card_bg` for all three skins
  (`theme.rs:53,78,103`) all exist.
- §3 chooser floor and scroll indicator — `chooser_band_height`
  (`ui.rs:274-300`) and `scroll_marker` (`ui.rs:859-866`).

Its header must be corrected to `implemented`. Re-planning that work because a
status line was never updated is exactly the failure `mur verify` exists to
prevent; consider whether `mur verify` should learn to flag a spec whose
"not yet implemented" claim is contradicted by the symbols it specifies.

---

## Deployment note

All three changes are visual and only reproduce in a real terminal. Unit tests
cover the arithmetic — `rebuild_after_resize` leaves the composer at
`rows - input_h - 1` for transcript lengths including zero; the composer ceiling
resolves to `viewport_h - 1`; the squeeze sets the marker; both render paths
emit identical `edit_diff_lines` output for the same edit — but the resize
behaviour itself must be confirmed by resizing an actual window, since the
defect is a reflow interaction the test harness does not have.
