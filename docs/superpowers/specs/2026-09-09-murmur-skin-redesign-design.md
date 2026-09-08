# murmur skins: three skins, one token vocabulary, fewer lines

**Status**: designed, not started.
**Field report**: the `mur` skin's secondary grey (`#7777aa`) sits at 4.6:1 on
its own background and lower on any other; `light` and `mur` rule a 60-column
line between every turn ("畫面好多條線"); the composer draws two rules with the
status bar's own rule directly under the second; and none of the three skins
follows the terminal's palette — a user who chose Catppuccin or Nord gets
murmur's colours on top of theirs. Two decisions were taken elsewhere and are
inputs here: the transcript band no longer draws a border (#1223), and the
theme fields are hand-filled per skin with nothing measuring them.

## Problem

`mur-core/src/cmd/agent/cli/theme.rs` defines `Theme` with 22 fields and three
constants (`DARK`, `LIGHT`, `MUR`). Four things follow from that shape:

1. **Nothing follows the terminal.** Every skin pins RGB values (`DARK` mixes
   named ANSI colours with `Rgb` greys). A terminal whose theme the user chose
   is painted over, and a truecolor skin on an unknown background can land
   text on a near-identical colour with no way to know.
2. **Six fields are one role.** `thinking`, `system`, `border_title`,
   `separator`, `border`, and the dim half of `user_text` are all "secondary
   grey", each hand-picked, each a different value, none measured. That is
   where the 4.6:1 came from.
3. **Lines carry no information.** The inter-turn rule marks a change of
   speaker that the role label (`you ›` / `● agent`) already marks. The
   composer's bottom border marks the same seam as the status bar under it.
   Best-practice reading (Hyperbliss 2026, terminfo.dev, clig.dev) converges:
   hierarchy comes from layout, weight, and symbols; colour reinforces; borders
   go to the focused panel only.
4. **No guard.** A colour edit that drops a token below readable contrast is
   invisible to CI.

## Decisions

Taken one at a time with the user on 2026-09-09; recorded here so the plan
does not re-open them.

| # | Decision |
|---|---|
| 1 | The third skin is `ansi`: ANSI-16 slots only, so the terminal's own theme decides every colour. |
| 2 | `ansi` is the new name; `dark` stays as an alias so `--skin dark` and saved config keep working. |
| 3 | `ansi` is the default skin when nothing is configured. |
| 4 | No inter-turn separator in any skin; `show_separator` is deleted. |
| 5 | The composer keeps its top rule (with the hint title) and drops the bottom one; the status bar sits directly under the input row. |
| 6 | `light` and `mur` assume a background (white; `#0b0b1a`) rather than painting one, and a unit test enforces WCAG contrast against it. |
| 7 | `Theme` collapses to semantic tokens (below). |
| 8 | `mur` keeps its purple/gold identity; only the greys and the reds/greens move. |

## Design

### 1. Token vocabulary

```rust
pub struct Theme {
    // text
    pub text: Style,        // body text — agent replies, user turns (user: DIM)
    pub muted: Style,       // metadata: notices, thinking, hints, rule titles, timestamps
    pub emphasis: Style,    // headings, focused items (bold)
    // identity
    pub accent: Style,      // "● agent" label, mascot, brand, focused-panel borders
    pub accent_alt: Style,  // "you ›" label, "$ cmd" shell label
    // status
    pub ok: Style,
    pub warn: Style,
    pub error: Style,
    // chrome
    pub border: Style,      // unfocused rules: composer top rule, table grid
    pub surface: Style,     // status bar and card background (bg only)
    pub badge: Style,       // agent-name / AUTO badge on the status bar
    // layout knobs
    pub border_type: BorderType,
    pub inner_padding: u8,
    pub compact_input: bool,
}
```

Tokens are `Style`, not `Color`: `ansi` needs modifiers (`DIM`, `BOLD`,
`REVERSED`) where the truecolor skins use a colour, and a call site must not
know which. Every use site in `ui/` and `markdown.rs` takes a token; none
composes one from a colour plus a modifier of its own.

Mapping from the old fields, for the structural PR:

| old | new |
|---|---|
| `agent`, `accent` | `accent` |
| `user`, `shell` | `accent_alt` |
| `agent_text` | `text` |
| `user_text` | `text` + `DIM` |
| `thinking`, `system`, `border_title` | `muted` |
| `success` / `warn` / `error` | `ok` / `warn` / `error` |
| `border` | `border` |
| `separator`, `show_separator` | deleted (decision 4) |
| `card_bg`, `status_bg` | `surface` |
| `badge_fg` + `badge_bg` | `badge` |

### 2. The three palettes

**`ansi`** (default; alias `dark`). No `Color::Rgb`, no `Color::Indexed`.

| token | value | why |
|---|---|---|
| `text` | `Reset` | the terminal's foreground |
| `muted` | `Reset` + `DIM` | not bright-black (ANSI 8): Solarized renders it invisible on dark |
| `emphasis` | `Reset` + `BOLD` | |
| `accent` | `Cyan` | today's `DARK` agent colour |
| `accent_alt` | `Green` | today's `DARK` user colour |
| `ok` / `warn` / `error` | `Green` / `Yellow` / `Red` | semantic slots every theme defines |
| `border` | `Reset` + `DIM` | |
| `surface` | `Reset` | no painted background |
| `badge` | `Cyan` + `REVERSED` + `BOLD` | reverse video keeps the terminal's own pair |

**`light`** — assumed background `#ffffff`. Ratios are WCAG 2 against it.

| token | value | ratio |
|---|---|---|
| `text` | `#1f2430` | 15.5 |
| `muted` | `#5c6370` | 6.0 |
| `emphasis` | `#000000` bold | 21.0 |
| `accent` | `#0b6e8f` | 5.8 |
| `accent_alt` | `#1a6b3a` | 6.5 |
| `ok` / `warn` / `error` | `#1a7f3a` / `#8a5a00` / `#b3261e` | 5.1 / 5.9 / 6.5 |
| `border` | `#c9ccd6` | decorative |
| `surface` | bg `#eef0f5` | decorative |
| `badge` | `#0b6e8f` on `#e0f0f8` | 4.9 |

**`mur`** — assumed background `#0b0b1a`; purple/gold kept.

| token | value | ratio |
|---|---|---|
| `text` | `#e4e4f4` | 15.5 |
| `muted` | `#9a9ac4` | 7.2 (was `#7777aa`, 4.6) |
| `emphasis` | `#ffffff` bold | 19.5 |
| `accent` | `#fbbf24` | 11.7 (unchanged) |
| `accent_alt` | `#b39dfb` | 8.5 (was `#a78bfa`, 7.2) |
| `ok` / `warn` / `error` | `#7fd48f` / `#f2c76a` / `#f28b98` | 10.9 / 12.2 / 8.3 |
| `border` | `#3f3f78` | decorative |
| `surface` | bg `#14142c` | status bar, cards |
| `badge` | `#fbbf24` on `#221a06` | 10.3 |

Ratios were computed with the WCAG 2 relative-luminance formula; the test in
§4 recomputes them, so the numbers above are documentation, not the source of
truth.

### 3. Layout

- **Inter-turn separator**: gone from `gap_row` — the gap before a message is
  a blank line in every skin. `wants_gap_before` keeps deciding *whether*
  there is a gap.
- **Composer**: `Borders::TOP` only, title unchanged. `INPUT_H_MIN` becomes
  2 (one text row plus the top rule); `render` and `chooser_band_height`
  already read the constant. The status bar draws no rule of its own, so
  it sits directly under the input row.
- **Focused panels** keep full borders and take `accent` on the border: the
  approval modal, the suggested-reply chooser, the slash-command popup.
  Unfocused rules (composer top, table grid, fleet rail) take `border`.
- **Transcript band**: no border (#1223). The scroll marker uses `muted`.
- **Status bar**: content unchanged; `surface` background, `badge` for the
  name and AUTO chips, `muted` for the rest, `warn`/`error` where it already
  colours degraded states.
- **Markdown tables** (#1226): grid in `border`, header in `emphasis`.

### 4. Guards

In `theme.rs`:

- `light_and_mur_meet_wcag`: for each of `light` and `mur`, every text token
  (`text`, `muted`, `emphasis`, `accent`, `accent_alt`, `ok`, `warn`, `error`,
  `badge` fg on `badge` bg) against the skin's assumed background: `text` ≥
  7.0, all others ≥ 4.5. The assumed background is a constant beside the
  palette so the test and the doc cannot drift.
- `ansi_pins_no_colour`: every token in `ansi` is `Reset` or one of the
  sixteen named `Color` variants; no `Rgb`, no `Indexed`.
- `dark_is_an_alias_of_ansi`: `resolve_skin("dark")` is `ansi` and
  `skin_name` of it is `"ansi"`.

In `ui/` (TestBackend renders, the pattern `band_growth_tests` set):

- `no_rule_between_turns`: a user turn followed by an agent turn under each
  skin paints no row that is mostly `─` between them.
- `composer_has_one_rule`: the frame has exactly one rule row above the
  input row and the status bar row directly under it.

### 5. Compatibility

- `resolve_skin`: `"ansi" | "dark"` → `ANSI`; `"light"`, `"mur"` unchanged;
  unknown → `ANSI` (was `DARK`). The unknown-skin notice lists
  `ansi, light, mur`.
- `skin_name(&ANSI)` returns `"ansi"`; `/skin` with no argument lists the
  three new names; `persist_skin` writes whatever name was typed, so a saved
  `dark` keeps working through the alias.
- `mur-common::config::Config.agent_cli.skin` doc and the `--skin` clap help:
  `ansi (default) | light | mur`.
- The Hub is not involved: skins are `mur agent cli`'s.

### 6. Delivery

Two PRs, so the visual change can be judged on its own diff.

**PR-1 — structure, zero visual change.** Collapse `Theme` to the token
struct; move every use site in `ui/`, `markdown.rs`, `welcome.rs`,
`fleet_rail.rs`, `render_card.rs`, `settlement.rs` onto tokens; fill the
three palettes with today's values. Where two old fields merge into one token
and held different values (`user_text`/`agent_text`, `thinking`/`system`/
`border_title`) the surviving value is the more frequent role's — the plan
lists each — so the render is identical for the agent turn, notices and the
status bar, and moves by at most that delta for user body text, thinking and
rule titles; a TestBackend test pins the identical cells and is deleted in
PR-2. `dark` alias and `ansi` name land here too, with `ansi` still holding
today's `DARK` values.

**PR-2 — the redesign.** New palettes (§2), layout (§3), guards (§4),
default `ansi` (§5), README skin paragraph and the docs-site skin section.

Not in scope, deliberately: user-authorable skins (TOML), OSC 11 background
detection, a fourth skin. Each is a separate decision when someone asks.

## Sources

- [Hyperbliss, "The Terminal Renaissance" (2026-04)](https://hyperbliss.tech/blog/2026.04.04_terminal-renaissance/) — token vocabulary, "usable at 16 colours, beautiful at true colour", borders for focus only.
- [terminfo.dev, colour fundamentals](https://terminfo.dev/fundamentals/color-fundamentals) — ANSI-16 as named slots the theme fills.
- [clig.dev](https://clig.dev/) — colour with intention; honour `NO_COLOR`.
- [MOLTamp, terminal colour schemes 2026](https://moltamp.com/blog/best-terminal-color-schemes-2026/) — 4.5:1 comfort, 7:1 for all-day.
- [ce9e, how (not) to build terminal colour schemes](https://blog.ce9e.org/posts/2019-06-24-terminal-colors/) — Solarized's 4.5 and why bright-black betrays it.
