# murmur UI pass: every colour through the theme, approvals docked above the composer, user skins

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
| 2 | `ansi` paints **no decorative background**: no zebra stripe, no code-block fill, no diff tint (the last is already so), no card surface. Reverse video is allowed in exactly two places: the **status row** (its chips — `badge`, `badge_warn` — which are fixed-position labels, not content) and the **one focused element** (the selected approval option, the selected menu row). Nowhere else. |
| 3 | New tokens: `surface_alt` (bg only; stripe) and `badge_warn` (AUTO chip). `READS` and `MONITOR` use the existing `badge`. No other new tokens. |
| 4 | User body text takes `muted` in every skin. `ansi`'s `muted` is DIM, so `ansi` renders as today; the RGB skins get a real, measured colour. RGB skins carry no `DIM` on any text token. |
| 5 | `/skin` applies immediately and prints one muted notice; scrollback is never cleared. |
| 6 | The approval panel is a **layout band docked above the composer**, like the suggested-reply chooser (#643); it is no longer a `Clear` overlay. |
| 7 | The countdown and the CLI's own expiry both read one `deadline` on the request, taken from the runtime's `timeout_ms`. No fleet special case: murmur's approval path never receives a deferred request (deferral lives on the fleet channel), so every request murmur shows has a deadline. |
| 8 | Options are uncoloured; the selected row is `badge`; only option 3 ("any tool") carries `warn` and `▲` — the glyph Warn notices already use; `⚠` renders two columns wide in many terminals and would push the panel's right border. |
| 9 | No "press n to add notes" feature. The reason for a denial is whatever is in the composer when the operator denies — the existing behaviour, now made visible in option 4's label. |
| 10 | Long diffs fold; the panel does not scroll internally (↑/↓ select options, PgUp scrolls the transcript — a third scroll owner would collide with both). |
| 11 | The role label shows the agent's `display_name`. |
| 12 | Colour marks exceptions, not the normal case: a successful tool row colours only its `✔`. |
| 13 | Consecutive tool rows stack with no blank row between them; the gap stays at every change of speaker. |
| 14 | A long run of tool rows is **not** collapsed into a summary line. Rows already in scrollback cannot be recollapsed (the scrollback limit of Problem 5), so a collapse would have to hide rows while they run. Revisit only if density is still a complaint after 12–13 ship. |
| 15 | Users can add skins as `<mur home>/skins/<name>.yaml` (§6). Built-in names cannot be shadowed. |
| 16 | A user skin names its base with a mandatory `inherits:` (a built-in, or `none`); unset tokens come from the base. |
| 17 | The file schema is an explicit whitelist split into required and optional keys; any other key is an error. |
| 18 | A skin that fails to load is rejected whole — never partly applied. At startup murmur falls back to `ansi`; a runtime `/skin` keeps the current skin. |
| 19 | The approve-then-fail-on-entitlement ordering (approving a write the agent can never perform) is behaviour, not UI: its own issue. |

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
| `surface_alt` | *(none)* | bg `#e9ecf2` | bg `#1a1a36` | bg `#2a2a2a` |
| `badge_warn` | `Yellow` + REVERSED + BOLD | `#8a5a00` on `#fbefd5` | `#0b0b1a` on `#f2c76a` | `#1a1a1a` on `#ffc107` |

`surface_alt` sits 1.16–1.21:1 from each assumed background — the visual
weight of today's `Indexed(236)` stripe (1.3–1.5:1), quieter. Every pair in
§6's contrast table passes with these values (checked 2026-09-29); §7's
guard is the source of truth.

Paint-site moves:

| site | today | becomes |
|---|---|---|
| `markdown.rs` `STRIPE` | `Indexed(236)` | `surface_alt` (const deleted) |
| `markdown.rs` `QUOTE` on the `▏` quote bar | `DarkGray` | `muted` |
| `markdown.rs` `QUOTE` on the `---` rule and the table grid | `DarkGray` | `border` (the 09-09 spec's "grid in `border`") |
| `markdown.rs` `HEADING` on list markers and table headers | `Cyan` | `accent` (+ BOLD on headers; identical on `ansi`) |
| `ui/status.rs` approval countdown | `Yellow` | `warn` |
| `ui/status.rs` monitor issue suffix | `Red` + BOLD | `error` + BOLD |
| `ui/status.rs` AUTO, AUTO:n | black on Yellow | `badge_warn` |
| `ui/status.rs` READS | black on Cyan | `badge` |
| `ui/status.rs` MONITOR | black on `Rgb(255,165,0)` | `badge` |
| `ui/hitl.rs`, `ui/chooser.rs`, `ui.rs` `DarkGray` | `DarkGray` | `muted` |
| `app/mod.rs` placeholder | `DarkGray` | `muted` |
| user body text | `text` + DIM | `muted` |
| `render_card.rs` error accent | `Color::Red` | `error` (see §4) |

`welcome.rs`'s `MascotMode::Accent(Rgb(0xfb,0xbf,0x24))` is the brand mark
on the `mur` skin only and stays. `markdown.rs` `CODE` (`Yellow`) stays
until the code-block design (out of scope below): it is a named slot, so it
breaks no `ansi` guarantee, and routing it now would pick a token the later
design may not keep. `ui/hitl.rs`'s option tints and `Yellow` command are
replaced wholesale by §3 and are not touched before it.

**Markdown needs the theme.** `markdown::render(src, width)` has no theme
today and its output is cached on the message (`ChatMsg.rendered`), so the
stripe cannot follow the skin without it. `render` gains a `theme`
argument; switching skin goes through one `App::apply_theme` that sets the
theme, re-renders cached markdown, and restyles the composer placeholder.

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
│   3 Yes · don't ask again for any tool                   ▲ │
│   4 No — type a reason below first, or Esc                 │
╰────────────────────────────────────────────────────────────╯
 ─ message · /help ─────────────────────────────────────────
   Type a message…
 MUR   approve edit_file
```

**Border.** Full border in `accent` (focused panel, per the 09-09 spec),
`border_type` from the skin.

**Title.** `<tool> · <target>` in `emphasis`; target is the path for file
tools, omitted when there is none. Right-aligned countdown `m:ss` to the
request's `deadline`, in `muted`, `warn` at ≤ 30 s.

**Deadline.** Today the CLI ignores the `timeout_ms` the runtime sends with
`tool/approval_needed` (`mur-agent-runtime/src/hitl/batch.rs`) and assumes
`gate::DEFAULT_TIMEOUT` (300 s) in four places: the status countdown
(`ui/status.rs`), the wake timer (`events.rs`), `expire_stale_hitl`
(`hitl.rs`) and the field doc on `HitlRequest` (`stream.rs`). This design:

- `HitlRequest` gains `deadline: Instant`, set once at receipt to
  `received_at + timeout_ms`. A notification without `timeout_ms` (a runtime
  predating it) gets `DEFAULT_TIMEOUT` — today's behaviour, now in one place.
- The panel countdown, the status line, the wake timer and
  `expire_stale_hitl` all read `deadline`; none of them names
  `DEFAULT_TIMEOUT` again.
- The CLI's clock starts at receipt, after the runtime's started at send, so
  the CLI always retires a request at or after the runtime has already
  denied it. A decision sent in that gap is refused by the runtime; nothing
  is approved late. Fail-closed, as today.
- No "no deadline" state is added; see decision 7.

**Body, by tool:**

| tool | body |
|---|---|
| `edit_file`, `write_file` (anything `diff::edit_diff_lines` accepts) | the diff, same rendering and tokens as the transcript's edit card |
| `bash` | the command, wrapped with the hanging indent of §2 |
| anything else | `key: value` rows, strings unescaped, each value truncated to one row with `…` |

**Height.** The panel is sized after the rows that are never given up —
status bar (1) and composer (its current height, 2 up to `INPUT_H_MAX` when
the operator has typed several lines) — and after the fleet rail at its
current height. What remains is `avail`. Then, in order, first that fits:

| form | rows | when |
|---|---|---|
| full | border + title + body + blank + 4 options | fits in `min(avail − 1, 60 % of terminal rows)` with the body folded to at least 3 rows |
| no body | border + title + 4 options = 7 | full does not fit; body replaced by `⋯ Ctrl+O shows the call` |
| compact | 2, no border: title row, then `1 Yes · 2 edit_file · 3 any tool ▲ · 4 No` | `avail` < 8 |
| status only | 0 | `avail` < 2: the status bar reads `approve edit_file — 1-4 · Ctrl+O` |

The transcript keeps at least one row whenever the full or no-body form is
used, and gives up its rows before the panel does in the compact form: the
decision outranks the history. Keys work identically in every form, so the
operator can always decide. While a gate is open the suggested-reply
chooser and the proposal chip are not drawn (they need the composer the
approval has taken over) and reappear after it closes.

A body that does not fit keeps the first and last hunks (or rows) and
replaces the middle with one `muted` row:
`⋯ 184 lines hidden · Ctrl+O shows all`.

**One surface.** Today an open gate shows on the step card's inline row and
falls back to the centred modal when that row is off screen
(`hitl_inline_visible`, `ui.rs`). The docked panel is always on screen, so
it becomes the only place a decision is made; the step card keeps its
`⏳ awaiting approval` state as a marker with no keys of its own. The
invariant in `ui.rs` — an open gate is always visible somewhere — holds by
construction, including the status-only form. Precondition for the plan: confirm
the Ctrl+O transcript view can show the pending request's full body; if it
cannot, the plan adds that before the panel ships.

**Options.** Rows in `text`; the selected row in `badge` (on `ansi` that is
reverse video — the focus exception of decision 2); option 3's `▲` and the
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


### 6. User skins (P4)

Proposed 2026-09-11, never built; decided here. A user skin is a file the
operator puts in place by hand. It is read once when selected and leaked to
`&'static`, so every frame costs what a built-in costs.

**Location and selection.**

- `<mur home>/skins/<name>.yaml`, where `<mur home>` is the `home` murmur
  already resolves for `config.yaml` (default `~/.mur`). `<name>` is the
  file stem, `[a-z0-9-]+`.
- Selected exactly like a built-in: `cli.skin: <name>`, `--skin <name>`,
  `/skin <name>`.
- Lookup is built-ins first, then `skins/`. A file named after a built-in
  (`ansi`, `dark`, `light`, `mur`, `clay`) is never loaded and is listed as
  invalid (`shadows a built-in skin`).
- The directory is scanned when a name is resolved and when `/skin` lists;
  there is no watcher and no hot reload.
- Nothing writes to `skins/` except the operator: no fleet, skill,
  `.muragent`, capability or official-catalog install carries or creates a
  skin file. A skin controls whether an approval is legible, so it is not
  content an import may bring along.

**File.**

```yaml
# ~/.mur/skins/dusk.yaml
inherits: mur              # required: a built-in skin name, or `none`
assumed_bg: "#101020"      # conditional, see below
tokens:
  accent: "#ff9e64"
  muted: "#a0a0c8"
  surface_alt: "on #16162e"
  badge_warn: "bold #101020 on #f2c76a"
layout:
  border_type: rounded
```

Top-level keys: `inherits`, `assumed_bg`, `tokens`, `layout`. Anything else
is an error.

**Whitelist.** The `tokens` and `layout` keys are exactly the fields of
`Theme`, split in two classes. With `inherits: <built-in>` every key is
optional and falls back to the base. With `inherits: none`, required keys
must be present, and optional keys that are absent are **derived** by the
rule in the right column — a fixed, documented function of the file's own
required tokens, never a value from some other skin.

| key | class | derived when absent under `inherits: none` |
|---|---|---|
| `text`, `muted`, `emphasis`, `accent`, `accent_alt`, `ok`, `warn`, `error`, `border`, `badge` | required | — |
| `surface`, `surface_alt`, `settlement_surface`, `diff_add_bg`, `diff_del_bg` | optional | none (no background) |
| `diff_add_mark` / `diff_del_mark` | optional | `ok` / `error` + bold |
| `diff_add_text` / `diff_del_text` | optional | `ok` / `error` when the matching `diff_*_bg` is absent, else `text` — the one-channel rule `theme.rs` documents |
| `settlement_text`, `_muted`, `_accent`, `_ok`, `_warn`, `_error` | optional | `text`, `muted`, `accent`, `ok`, `warn`, `error` |
| `badge_warn` | optional | `warn` + reversed + bold |
| `layout.border_type` | optional | `plain` (`plain` \| `rounded` \| `double` \| `thick`) |
| `layout.inner_padding` | optional | `1` (0–4) |
| `layout.compact_input` | optional | `false` |

The whitelist lives next to `Theme` as one table the parser and the
`/skin` listing both read, so adding a `Theme` field without a whitelist row
fails a test (below), not a user.

**Value syntax** for a token: `[modifier …] [fg] [on bg]`, space-separated,
case-insensitive.

- modifier: `bold`, `dim`, `italic`, `underlined`, `reversed`
- colour: `#rrggbb`; one of the sixteen named slots `black red green yellow
  blue magenta cyan gray dark-gray light-red light-green light-yellow
  light-blue light-magenta light-cyan white`; or `default` (the terminal's
  own)
- `none` alone: the empty style (clears an inherited value)

Every value parses into a typed `Style`; no string reaches the terminal, so
there is no escape-sequence path.

**`assumed_bg`.** Required when the resolved skin — after inheritance and
derivation — contains any `#rrggbb` colour. Resolved from the file, else the
base's assumed background (`light`, `mur`, `clay` have one; `ansi` does
not), else it is an error. A skin whose colours are all named slots or
`default` needs none and is not contrast-checked, the same reasoning as the
built-in `ansi`.

**Contrast check at load** measures every foreground against the
background it is actually painted on, not against `assumed_bg` alone. A
token's effective background is its own `bg` if it has one, else the
surface named in this table, else `assumed_bg`.

| foreground | on | min |
|---|---|---|
| `text` | `assumed_bg`, `surface`, `surface_alt` | 7:1 |
| `muted`, `emphasis`, `accent`, `accent_alt`, `ok`, `warn`, `error` | `assumed_bg`, `surface`, `surface_alt` | 4.5:1 |
| `diff_add_text` / `diff_del_text` | `diff_add_bg` / `diff_del_bg` | 4.5:1 |
| `diff_add_mark` / `diff_del_mark` | `diff_add_bg` / `diff_del_bg` | 3:1 (a glyph, WCAG 1.4.11) |
| `settlement_text` | `settlement_surface` | 7:1 |
| `settlement_muted`, `_accent`, `_ok`, `_warn`, `_error` | `settlement_surface` | 4.5:1 |
| `badge`, `badge_warn` | their own `bg` | 4.5:1 |

A surface that is unset collapses to `assumed_bg`, so the pairs are always
defined. `dim` is modelled as the colour blended 50 % toward the background
it is on — the A7 gap closed for user files too. A pair where either side is
a named slot or `default` is skipped. The same function runs over the
built-in skins in `rgb_skins_meet_wcag`, so built-ins and user files meet
one rule; the plan adjusts any built-in value that fails it.

**Failure.** Load errors are: YAML that does not parse, an unknown key, a
malformed value, a missing required key under `none`, a missing
`assumed_bg`, a contrast failure, a shadowing name, an unknown `inherits`.
All are collected, then the skin is rejected whole.

| when | result | notice (`warn`, one row) |
|---|---|---|
| startup (`cli.skin`, `--skin`) | `ansi` | `▲ skin dusk not loaded: tokens.warn 2.1:1 on #101020 (needs 4.5:1) (+3 more) — using ansi` |
| `/skin dusk` | current skin kept; config not written | `▲ skin dusk not loaded: … (+3 more) — keeping mur` |

`(+N more)` appears only when there is more than one error. The full list is
in `/skin` with no argument, which lists built-ins then user skins; an
invalid file is shown as `dusk (invalid) — <first error>` and any further
errors indented under it.

`persist_skin` writes the name only after a successful load.

Ceiling, recorded: each successful user-skin load leaks one `Theme`
(~1 KB). Bounded by how often an operator types `/skin`; revisit only if a
reload loop ever exists.

### 7. Guards

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
- `approval_countdown`: `warn` at 30 s left, `muted` at 31 s.
- `deadline_from_runtime_timeout`: a notification with `timeout_ms: 90000`
  gets a 90 s deadline; the panel shows `1:30`; `expire_stale_hitl` retires
  it after 90 s and not at 89 s; the wake timer is armed for 90 s.
- `deadline_legacy_default`: a notification without `timeout_ms` gets
  `DEFAULT_TIMEOUT`.
- `no_default_timeout_outside_receipt` (source-level): `DEFAULT_TIMEOUT` is
  referenced only where `deadline` is set.
- `approval_height_caps_and_folds`: a 400-line diff in a 40-row terminal
  yields a panel of ≤ 24 rows containing the `lines hidden` row.
- `approval_forms_by_height`: with a 1-row composer and no rail, terminals
  of 40 / 12 / 8 / 5 / 4 rows produce the full / no-body / compact / compact
  / status-only form; every frame shows the four choices or the status-only
  hint, and the status bar is on the last row.
- `approval_with_multiline_composer`: a 12-row terminal with a 5-line
  composer drops to the compact form, the composer keeps all 5 lines.
- `approval_with_fleet_rail`: the rail's rows are subtracted before the
  panel is sized.
- `approval_hides_chooser_and_chip`: with suggested replies and a proposal
  pending, opening a gate removes both bands; closing it restores them.
- `approval_single_surface`: a gate whose step card is on screen renders
  the panel and no inline decision row.
- `ansi_render_has_no_rgb`: a frame with a table, the approval panel and
  the status bar under `ansi` contains no `Rgb` or `Indexed` cell colour.
- `ansi_reverse_only_status_and_focus`: in the same frame, every
  `REVERSED` cell is on the status row or on the selected option's row.
- `contrast_pairs_catch_hidden_diff` (user skin): `inherits: mur` with
  `diff_add_bg: "on #e4e4f4"` is rejected naming
  `diff_add_text on diff_add_bg`.
- `contrast_pairs_catch_stripe`: a `surface_alt` equal to `text`'s colour
  is rejected naming `text on surface_alt`.
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

User skins (§6), parser-level unless noted:

- `whitelist_covers_theme`: every `Theme` field has exactly one whitelist
  row, and every row names a field.
- `whitelist_classes`: under `inherits: none`, a file with only the ten
  required tokens loads and every optional token equals its derivation; a
  file missing any one required token fails naming it.
- `none_with_bg_but_missing_required`: `inherits: none`, `assumed_bg` set,
  `warn` absent → rejected, error names `tokens.warn`.
- `inherits_fills_unset`: `inherits: mur` + one `accent` override equals
  `MUR` in every other field.
- `unknown_key_rejected`: at top level, under `tokens`, under `layout`.
- `value_syntax`: `bold #d97757 on #262626`, `on #16162e`, `dim cyan`,
  `default`, `none` parse to the expected `Style`; `#12345`, `blod red`,
  `red on` are errors.
- `layout_override`: `border_type: double`, `inner_padding: 2`,
  `compact_input: true` reach the loaded `Theme`; `inner_padding: 9` is an
  error.
- `assumed_bg_resolution`: RGB under `inherits: mur` with no `assumed_bg`
  loads (uses `#0b0b1a`); RGB under `inherits: ansi` or `none` without it
  is rejected; named-slot-only under `none` loads without it.
- `contrast_rejects_invisible_ink`: `warn` equal to `assumed_bg` is
  rejected with the measured ratio in the error; `dim` on a passing colour
  that falls under 4.5 after the blend is rejected.
- `cannot_shadow_builtin`: `skins/mur.yaml` is never loaded; `/skin mur`
  gives the built-in.
- `multiple_errors_first_plus_count`: three errors → notice holds the first
  and `(+2 more)`.
- `startup_broken_falls_back_to_ansi` (term setup): `cli.skin` naming a
  broken file starts on `ansi` with the startup notice.
- `runtime_broken_keeps_current` (slash): under `mur`, `/skin broken`
  leaves `app.theme` on `MUR`, prints `keeping mur`, and leaves
  `config.yaml` unchanged.
- `skin_list_shows_user_skins`: `/skin` lists a valid user skin by name and
  a broken one as `(invalid)` with its first error.

### 8. Delivery

Three PRs, each reviewable on its own screenshot:

1. **Tokens.** §1 and the guards on tokens. Visual change limited to the
   status chips, the stripe, and user text colour on RGB skins.
2. **Text rendering.** Hanging indent, CJK emphasis, `/skin` notice, role
   label, and the tool-row ink / stacking / duration / intent of §4 — the
   change the 30-call screenshot asks for, independent of the approval
   panel.
3. **Approval panel and failure rows.** §3 — including the `deadline`
   plumbing and the single-surface change — and §4's one-line failure,
   `fix:` row and denial row.
4. **User skins.** §6. Last, because its whitelist is `Theme`'s field list
   and must see PR-1's `surface_alt` and `badge_warn`. User-facing: README
   skin section, the docs-site `/skin` page (the `update-docs` skill), and
   `--skin` / `config.cli.skin` help naming `<mur home>/skins/`.

Not in scope: a `mur skin` subcommand (validate, scaffold, export) — `/skin`
listing reports errors; OSC 11 background detection; skins shipped by
anything other than the operator. Also not in scope: composer hint text and the status bar's other fields
(`⏵ 534 · 01a0eae7`, `0/… tok`); code-block and inline-code styling;
the welcome layout; the approve-then-entitlement-failure ordering. Each is
its own design.
