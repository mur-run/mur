# murmur UI pass: every colour through the theme, approvals docked above the composer

**Status**: designed, not started.
**Field report**: a computer-use walk of one transcript (HITL approval, a
failed tool call, markdown table, code block, CJK headings) under all four
skins, 2026-09-29, each skin on the background it assumes (`ansi` also on a
light terminal). Builds on `2026-09-09-murmur-skin-redesign-design.md`, whose
token vocabulary and decisions stand.

## Problem

Seen on screen, grouped by cause rather than by symptom:

1. **Colours that bypass the theme.** The table zebra stripe is
   `Color::Indexed(236)` (`markdown.rs` `STRIPE`); on a light terminal under
   `ansi` it is a near-black bar and the striped rows' text disappears. The
   status-bar chips are pinned — `AUTO` black-on-yellow, `READS`
   black-on-cyan, `MONITOR` black-on-`Rgb(255,165,0)` (`ui/status.rs`) — so
   `mur` and `clay` each carry a cyan slab that belongs to no skin. Nine more
   paint sites use `Color::DarkGray` directly (`ui/hitl.rs`, `ui/chooser.rs`,
   `ui.rs`, the textarea placeholder in `app/mod.rs`).
2. **Wrapped lines lose their indent.** A message body sits under its role
   label at `MSG_INDENT`; its second and later wrapped rows start at column
   0. Worst in CJK paragraphs, which have no spaces and wrap often. Same for
   the tool-error line.
3. **CJK bold renders literally.** `。**33f30194 …確認。**上一輪` shows
   the `**`: under CommonMark's flanking rules a closing `**` preceded by
   CJK punctuation and followed by a CJK letter is not right-flanking, so
   pulldown-cmark leaves it as text.
4. **User turns are too faint on `light`.** User body text is `text` + `DIM`;
   DIM on white lands far under 4.5:1. The WCAG guard measures `text` and
   never sees the modifier.
5. **`/skin` mixes palettes.** Settled messages are written into the
   terminal's own scrollback by `Terminal::insert_before` (`ui/band.rs`).
   Once there, murmur cannot repaint them, so switching skin leaves old rows
   in the old palette. This is the terminal's limit, not a render bug.
6. **The approval modal hides what it asks about.**
   - It floats centred over the transcript, cutting the agent's last lines
     mid-glyph — the lines that explain *why* it wants the tool.
   - `edit_file` is shown as raw JSON (`"old_string": "hello\nsecond line"`)
     although `diff.rs` already renders edit diffs.
   - The countdown lives only in the status bar.
   - The four options are green / yellow / magenta / red with no meaning
     behind the colours; the riskiest one ("any tool") is not the one that
     stands out.
7. **One failed tool call is three red lines.** `✔ approved edit_file · …`,
   `✗ edit_file … · 19ms`, `✗ tool error: tool execution failed: …` — the
   error text is repeated and the whole block reads as an alarm.
8. **A busy turn paints the page in the accent colour.** A 30-call turn
   (user screenshot, 2026-09-29) is 30 success rows whose whole header —
   glyph, tool name, command — is `accent` + BOLD (`render_card.rs`
   `card_lines`), each followed by a blank row (`ui/band.rs` `message_block`
   puts `gap_row` before every message). Success is the normal case, so the
   loudest ink goes to the least informative rows, and the agent's own
   sentences between them — the part the operator reads — drown. Three
   smaller defects on the same rows: the error state is a pinned
   `Color::Red`; durations print raw milliseconds (`126076ms`); and the
   intent note is middle-elided (`(Branching and…t and`), which reads as
   garbage.
9. **The agent is anonymous.** Every reply is headed `● agent`
   (`ui/message.rs` `push_agent_header`), including in split panes where
   several agents talk at once.

Checked and **not** a problem: both overlays already `Clear` before drawing
(`ui.rs` slash popup, `ui/hitl.rs`). What looked like bleed-through was the
centred modal cutting the transcript beside it, which §3 removes.

## Decisions

Taken with the user on 2026-09-29.

| # | Decision |
|---|---|
| 1 | One design for colour cleanup and the approval panel, so the panel is born on tokens. |
| 2 | `ansi` paints **no decorative background**: no zebra stripe, no code-block fill, no diff tint (the last is already so). Reverse video is allowed for **focus only** — one element at a time. |
| 3 | New tokens: `surface_alt` (bg only; stripe) and `badge_warn` (AUTO chip). `READS` and `MONITOR` use the existing `badge`. No other new tokens. |
| 4 | User body text takes `muted` in every skin. `ansi`'s `muted` is DIM, so `ansi` renders as today; the RGB skins get a real, measured colour. RGB skins carry no `DIM` on any text token. |
| 5 | `/skin` applies immediately and prints one muted notice; scrollback is never cleared. |
| 6 | The approval panel is a **layout band docked above the composer**, like the suggested-reply chooser (#643); it is no longer a `Clear` overlay. |
| 7 | Countdown is shown whenever the request carries a deadline; deferred requests have none and show none. No fleet special case. |
| 8 | Options are uncoloured; the selected row is `badge`; only option 3 ("any tool") carries `warn` and `⚠`. |
| 9 | No "press n to add notes" feature. The reason for a denial is whatever is in the composer when the operator denies — the existing behaviour, now made visible in option 4's label. |
| 10 | Long diffs fold; the panel does not scroll internally (↑/↓ select options, PgUp scrolls the transcript — a third scroll owner would collide with both). |
| 11 | The role label shows the agent's `display_name`. |
| 12 | Colour marks exceptions, not the normal case: a successful tool row colours only its `✔`. |
| 13 | Consecutive tool rows stack with no blank row between them; the gap stays at every change of speaker. |
| 14 | A long run of tool rows is **not** collapsed into a summary line. Rows already in scrollback cannot be recollapsed (the scrollback limit of Problem 5), so a collapse would have to hide rows while they run. Revisit only if density is still a complaint after 12–13 ship. |
| 15 | The approve-then-fail-on-entitlement ordering (approving a write the agent can never perform) is behaviour, not UI: its own issue. |

## Design

### 1. Tokens (P1)

Added to `Theme`:

```rust
/// Faint alternate-row background (table zebra stripe). bg only.
/// `ansi`: empty — it never learns the terminal's background.
pub surface_alt: Style,
/// A status chip that means "risk is on": AUTO / AUTO:<tools>.
pub badge_warn: Style,
```

| token | ansi | light | mur | clay |
|---|---|---|---|---|
| `surface_alt` | *(none)* | bg `#f4f5f8` | bg `#12122a` | bg `#212121` |
| `badge_warn` | `Yellow` + REVERSED + BOLD | `#8a5a00` on `#fbefd5` | `#0b0b1a` on `#f2c76a` | `#1a1a1a` on `#ffc107` |

The RGB values are starting points; §6's guard is the source of truth and
the plan adjusts any that fail it.

Paint-site moves:

| site | today | becomes |
|---|---|---|
| `markdown.rs` `STRIPE` | `Indexed(236)` | `surface_alt` (const deleted) |
| `markdown.rs` `QUOTE` | `DarkGray` | `muted` |
| `ui/status.rs` AUTO, AUTO:n | black on Yellow | `badge_warn` |
| `ui/status.rs` READS | black on Cyan | `badge` |
| `ui/status.rs` MONITOR | black on `Rgb(255,165,0)` | `badge` |
| `ui/hitl.rs`, `ui/chooser.rs`, `ui.rs` `DarkGray` | `DarkGray` | `muted` |
| `app/mod.rs` placeholder | `DarkGray` | `muted` |
| user body text | `text` + DIM | `muted` |
| `render_card.rs` error accent | `Color::Red` | `error` (see §4) |

`welcome.rs`'s `MascotMode::Accent(Rgb(0xfb,0xbf,0x24))` is the brand mark
on the `mur` skin only and stays.

### 2. Rendering fixes (P1)

- **Hanging indent.** Body text is wrapped by our code, not by
  `Paragraph::wrap`, with continuation rows prefixed by the same indent as
  the first row (`MSG_INDENT`, plus list-marker width inside lists).
  `markdown.rs` `wrap_spans` is the existing wrapper; the band's physical-row
  count (`ui/band.rs`, which must match the wrapped height exactly) reads the
  same wrap. The tool-error detail line gets the same treatment.
- **CJK emphasis.** A post-pass over parsed text events: a `Text` event that
  still contains a `**…**` pair on one paragraph, where the pair sits against
  CJK punctuation, is re-split into bold spans. It acts only on what the
  parser already left literal, so ordinary CommonMark is untouched. The plan
  starts with a spike to confirm pulldown-cmark 0.12 delivers those
  delimiters as plain text rather than splitting them.
- **`/skin` notice.** On an operator-typed `/skin <name>` only (not on
  startup, not on `--skin`):
  `skin → light (earlier lines keep their colours; murmur --resume repaints them)`
  in `muted`.

### 3. The approval panel (P2)

A band between the transcript and the composer, sized by the same mechanism
as `chooser_band_height`. The transcript band shrinks to make room; nothing
is drawn over it.

```
╭ edit_file · ~/murui-demo/hello.md ────────────────── 4:52 ╮
│ @@ -1,2 +1,2 @@                                            │
│ ▌- hello                                                   │
│ ▌+ hello world                                             │
│    second line                                             │
│                                                            │
│ ▸ 1 Yes                                                    │
│   2 Yes · don't ask again for edit_file                    │
│   3 Yes · don't ask again for any tool                   ⚠ │
│   4 No — type a reason below first, or Esc                 │
╰────────────────────────────────────────────────────────────╯
 ─ message · /help ─────────────────────────────────────────
   Type a message…
 MUR   approve edit_file
```

**Border.** Full border in `accent` (focused panel, per the 09-09 spec),
`border_type` from the skin.

**Title.** `<tool> · <target>` in `emphasis`; target is the path for file
tools, omitted when there is none. Right-aligned countdown `m:ss` in `muted`,
`warn` at ≤ 30 s, absent when the request has no deadline.

**Body, by tool:**

| tool | body |
|---|---|
| `edit_file`, `write_file` (anything `diff::edit_diff_lines` accepts) | the diff, same rendering and tokens as the transcript's edit card |
| `bash` | the command, wrapped with the hanging indent of §2 |
| anything else | `key: value` rows, strings unescaped, each value truncated to one row with `…` |

**Height.** At most 60 % of the terminal's rows, and never less than the
four options plus title and border. A body that does not fit keeps the first
and last hunks (or rows) and replaces the middle with one `muted` row:
`⋯ 184 lines hidden · Ctrl+O shows all`. Precondition for the plan: confirm
the Ctrl+O transcript view can show the pending request's full body; if it
cannot, the plan adds that before the panel ships.

**Options.** Rows in `text`; the selected row in `badge` (on `ansi` that is
reverse video — the focus exception of decision 2); option 3's `⚠` and the
words "any tool" in `warn`. Option 4's label follows the composer:

- empty: `No — type a reason below first, or Esc`
- non-empty: `No — send "<first 30 chars>…" as the reason`

Keys are unchanged: ↑/↓, Enter, `1`–`4` and `n` when the composer is empty,
Esc. The footer hint row (`↑/↓ select · Enter confirm · …`) is dropped: the
numbers are on the rows, and Esc is on option 4.

**Status bar.** Shows `approve <tool>` only; the countdown moved into the
panel.

### 4. Tool event lines (P2)

One line per call, the approval folded in:

```
✔ bash ls ~/murui-demo · 4 lines · 15ms  (Listing demo dir)
✗ edit_file ~/murui-demo/hello.md · 19ms — path not write-entitled
    fix: mur agent perm allow-write mur ~/murui-demo/hello.md && mur agent restart mur
⊘ edit_file ~/murui-demo/hello.md — denied
```

- The separate `✔ approved <tool>` line is dropped; an approved call is
  simply a call that ran. A denial keeps its own `⊘ … — denied` line because
  nothing else records it.
- A failure's summary is the error's innermost cause, not the
  `tool error: tool execution failed:` wrapper chain. Only the glyph and tool
  name take `error`; the summary takes `text`.
- A remediation hint the runtime already embeds (`grant it via \`…\``) is
  lifted onto an indented `muted` `fix:` row. Nothing is sent to a log
  instead: the command is what the operator needs next.
- Session-allow bookkeeping lines (`approved bash · …` verb/badge split in
  `ui/message.rs`) are unchanged.

**Ink, per span of a tool row** (`render_card.rs` `card_lines`):

| span | state | today | becomes |
|---|---|---|---|
| glyph | done `✔` | `accent` + BOLD | `ok` |
| glyph | error `✗` | `Color::Red` | `error` + BOLD |
| glyph | running `◐`, yielded `⏳` | `accent` + BOLD | `accent` |
| tool name | any | `accent` + BOLD | `muted`; `error` + BOLD on error |
| command / arg hint | any | `accent` + BOLD | `text` |
| `→ gist`, duration, `[auto]` | any | `muted` | `muted` (unchanged) |
| intent note | any | `muted` + DIM | `muted` (DIM goes with decision 4) |

Nothing on a successful row is bold, so on a page of them the eye lands on
the `●` role label and the agent's sentences.

**Stacking.** `message_block` skips the gap row when both the previous
message and this one are step cards (`m.step.is_some()`). Measured and
painted blocks share `message_block`, so the band's row count stays exact.
A system notice, a user turn or an agent turn between two cards restores the
gap on both sides as today.

**Duration.** One formatter for the card header and the `dump.rs` line:
`< 1 s` → `640ms`; `< 60 s` → `12.4s`; otherwise `2m06s`.

**Intent note.** Tail-elided at a word boundary — `elide_tail_at_word`'s
rule, budgeted in display columns as `elide_middle_cols` is (the existing
helper counts bytes, which over-cuts CJK) — not middle-elided: the model's
sentence keeps its opening and loses its end. Below `INTENT_MIN_COLS` it is
omitted, as today.

```
✔ bash cd mur-agent-runtime/src; grep -n "HitlRespond…   → 17 lines · 37ms
✔ bash cd mur-agent-runtime/src; python3 - <<'EOF'…      → 18 lines · 66ms
✔ bash cargo test -q -p mur-agent-runtime --locked …     → 30 lines · 2m06s  (Running the…)

● MUR
  supervisor_shutdown 掛了。我先用 mur-debugging 的做法…
```

### 5. Role label (P2)

`push_agent_header` takes the agent's `display_name` (`● MUR`, streaming:
`⠋ MUR`). `you ›` is unchanged.

### 6. Guards

In `theme.rs`:

- The existing `rgb_skins_meet_wcag` (already covers `light`, `mur`,
  `clay`) adds `badge_warn` fg on bg.
- `rgb_skins_carry_no_dim`: no text token of `light`, `mur`, `clay` has
  `Modifier::DIM`.
- `ansi_paints_no_decorative_bg`: `surface`, `surface_alt`, `diff_*_bg` of
  `ansi` have no `bg`.

In `ui/` (TestBackend, the `band_growth_tests` pattern):

- `approval_band_is_docked`: with a pending request, no transcript row and
  no panel row share a screen row, and the composer sits directly under the
  panel.
- `approval_shows_diff_for_edit`: an `edit_file` request renders `▌-`/`▌+`
  rows and no `"old_string"`.
- `approval_countdown`: absent without a deadline; `warn` at 30 s; `muted`
  at 31 s.
- `approval_height_caps_and_folds`: a 400-line diff in a 40-row terminal
  yields a panel of ≤ 24 rows containing the `lines hidden` row.
- `ansi_render_has_no_rgb`: a frame with a table, the approval panel and
  the status bar under `ansi` contains no `Rgb` or `Indexed` cell colour.
- `wrapped_body_keeps_indent`: a CJK paragraph wider than the pane; every
  continuation row starts with `MSG_INDENT`.
- `cjk_bold`: the example from Problem 3 renders `33f30194 這次 …確認。`
  bold and no `**`.
- `failed_tool_is_one_summary_line`: the edit_file failure above renders
  one `✗` row plus one `fix:` row.
- `success_row_colours_only_the_glyph`: under each skin, a done card's
  header has `ok` on the `✔` cell and no cell carrying `accent`, `ok` or
  BOLD elsewhere on the row.
- `consecutive_cards_stack`: card, card, agent turn, card renders no blank
  row between the first two cards and one blank row on each side of the
  agent turn; the measured height equals the painted height.
- `duration_formats`: `640` → `640ms`, `12_400` → `12.4s`,
  `126_076` → `2m06s`.
- `intent_is_tail_elided`: a long intent keeps its first words and ends in
  `…)`; it never contains `…` followed by more text.

### 7. Delivery

Three PRs, each reviewable on its own screenshot:

1. **Tokens.** §1 and the guards on tokens. Visual change limited to the
   status chips, the stripe, and user text colour on RGB skins.
2. **Text rendering.** Hanging indent, CJK emphasis, `/skin` notice, role
   label, and the tool-row ink / stacking / duration / intent of §4 — the
   change the 30-call screenshot asks for, independent of the approval
   panel.
3. **Approval panel and failure rows.** §3, and §4's one-line failure,
   `fix:` row and denial row.

Not in scope: composer hint text and the status bar's other fields
(`⏵ 534 · 01a0eae7`, `0/… tok`); code-block and inline-code styling;
the welcome layout; the approve-then-entitlement-failure ordering. Each is
its own design.
