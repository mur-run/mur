# murmur skin redesign — implementation plan

> **Execute with `mur-executing-plans`** (in-context, task by task). No MUR
> delegation is set up for this branch.

**Spec:** `docs/superpowers/specs/2026-09-09-murmur-skin-redesign-design.md`
**Branches:** PR-1 on `refactor/murmur-theme-tokens`, PR-2 on
`feat/murmur-skin-redesign` (branched from main after PR-1 merges); spec and
this ledger on `docs/murmur-skin-redesign-spec` (PR #1229).
**Status:** PR-1 done and live-checked; PR opened. PR-2 not started.

## Corrections found during execution

1. **`#![allow(dead_code)]` stays in `theme.rs` for PR-1** (Task 1). `emphasis` has no paint site until the redesign PR, and clippy runs with `-D warnings`.
2. **The pin test finds cells by text, not by coordinate** (Task 4): the welcome header sits above message 0 in the band, so fixed coordinates would have pinned the mascot. `welcome_dismissed = true` and a `find("● agent")` helper locate the cells.
3. **A finished reply's body never took `agent_text`** (Task 4). Markdown-rendered prose carries the terminal's own foreground; only chooser rows, cards and the settlement used the old field. The pin asserts `Color::Reset` for the body and PR-2 decides whether `text` should reach the renderer (spec §1 says body text is `text`).
4. **Sending `/skin` + Enter in tmux accepts the completion menu's first row** (`ansi`) and switches the live skin — the live check must type `/skin ` with a trailing space or read the mascot colour with no keystrokes. This also persisted `cli.skin: ansi` into the reporting machine's config.
5. **`/skin` completion rows and the startup fallback named `dark`** (Task 3, not in the plan): `complete.rs` `SKINS` now lists `ansi` first; `mod.rs` falls back to `"ansi"`. Both resolve to the same theme in PR-1.

## Deviation from the spec, decided while planning

Spec §6 promised PR-1 a "byte-identical" render. That cannot hold where two old
fields merge into one token and held different values (`DARK.border_title`
`#707070` vs `DARK.system` `#8a8a8a`; `LIGHT.thinking` `#888899` vs
`LIGHT.system` `#777788`; `DARK.user_text` `#b8b8b8` vs `DARK.agent_text`
`#eaeaea`). PR-1 therefore keeps the *surviving* value per token (the table in
Task 1) and its guard is the existing suite plus a render test that pins the
agent turn, notice, and status bar — the cells whose colours are 1:1. User
body text, thinking text, rule titles and the inter-turn rule are allowed to
move by at most the delta in that table, and PR-2 moves them anyway.

## Goal

Three skins — `ansi` (follows the terminal; alias `dark`; default), `light`,
`mur` — defined as one semantic token vocabulary with contrast enforced by
test, and a transcript with no inter-turn rules and a single rule above the
composer.

## Architecture

`theme::Theme` becomes eleven `ratatui::style::Style` tokens plus three layout
knobs. Every paint site in `cmd/agent/cli/` takes a token by name and never
composes colour + modifier itself, so `ansi` can express "muted" as `DIM` where
`light` uses a grey. PR-1 swaps the struct and every use site while filling the
palettes with today's values; PR-2 fills the final palettes, removes the rules,
and adds the guards.

## Tech stack

Rust 2024, ratatui 0.29 (`Style`, `Modifier`, `Color`, `BorderType`), the
existing `TestBackend` render tests in `ui/band/tests.rs`, `cargo nextest`.

## Global Constraints

- Tokens are `Style`, not `Color`: `ansi` needs modifiers (`DIM`, `BOLD`, `REVERSED`) where the truecolor skins use a colour, and a call site must not know which. Every use site in `ui/` and `markdown.rs` takes a token; none composes one from a colour plus a modifier of its own.
- `ansi`: No `Color::Rgb`, no `Color::Indexed`.
- `light` and `mur` assume a background (white; `#0b0b1a`) rather than painting one, and a unit test enforces WCAG contrast against it: `text` ≥ 7.0, all others ≥ 4.5.
- No inter-turn separator in any skin; `show_separator` is deleted.
- `dark` stays as an alias so `--skin dark` and saved config keep working.
- Single source file ≤ 800 lines (CLAUDE.md rule 4). `theme.rs` grows; keep its tests in `theme/tests.rs` if it passes 700.
- Brand name is uppercase "MUR" in anything a user reads (CLAUDE.md rule 7); skin *names* are lowercase identifiers.
- Build/test environment: `MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download` on every `cargo` invocation; tests via `cargo nextest run -p mur-core --lib …` with `RUST_MIN_STACK=33554432`; lint via `cargo clippy -p mur-core --all-targets -- -D warnings` and `cargo fmt --check -p mur-core`.

## File structure

| file | change | responsibility |
|---|---|---|
| `mur-core/src/cmd/agent/cli/theme.rs` | rewrite | `Theme` tokens, three palettes, `resolve_skin` with alias, `skin_name`, `is_known_skin`, `SKIN_NAMES`, contrast + ansi guards (PR-2) |
| `mur-core/src/cmd/agent/cli/ui/message.rs` | edit | role headers, user body, notices, shell blocks; `gap_row` loses the rule (PR-2) |
| `mur-core/src/cmd/agent/cli/ui/band.rs` | edit | scroll marker, stage-2 agent header |
| `mur-core/src/cmd/agent/cli/ui/chooser.rs` | edit | chooser rows and border |
| `mur-core/src/cmd/agent/cli/ui/rail.rs` | edit | rail member states |
| `mur-core/src/cmd/agent/cli/ui/status.rs` | edit | badge, ready/generating text |
| `mur-core/src/cmd/agent/cli/ui.rs` | edit | completion popup; `INPUT_H_MIN` 3→2 (PR-2) |
| `mur-core/src/cmd/agent/cli/render_card.rs` | edit | step cards |
| `mur-core/src/cmd/agent/cli/settlement.rs` | edit | settlement card |
| `mur-core/src/cmd/agent/cli/diff.rs` | edit | diff header/footer |
| `mur-core/src/cmd/agent/cli/welcome.rs` | edit | mascot colour, identity/hint lines |
| `mur-core/src/cmd/agent/cli/app.rs` | edit | composer block (top rule only in PR-2) |
| `mur-core/src/cmd/agent/cli/mod.rs` | edit | `/skin` notices name `ansi, light, mur`; unknown-skin fallback |
| `mur-core/src/cli/agent.rs` | edit | `--skin` help text |
| `mur-common/src/config.rs` | edit | `CliConfig.skin` doc |
| `mur-core/src/cmd/agent/cli/ui/band/tests.rs` | edit | PR-1 pin test; PR-2 layout guards |
| `README.md` | edit | skin paragraph (PR-2) |

---

# PR-1 — structure, palettes hold today's values

## Task 1 — `Theme` becomes tokens; `ansi` name, `dark` alias

**Interfaces — Produces:**

```rust
// mur-core/src/cmd/agent/cli/theme.rs
pub struct Theme {
    pub text: Style, pub muted: Style, pub emphasis: Style,
    pub accent: Style, pub accent_alt: Style,
    pub ok: Style, pub warn: Style, pub error: Style,
    pub border: Style, pub surface: Style, pub badge: Style,
    pub border_type: BorderType, pub inner_padding: u8, pub compact_input: bool,
}
pub const ANSI: Theme; pub const LIGHT: Theme; pub const MUR: Theme;
pub const SKIN_NAMES: &str = "ansi, light, mur";
pub fn resolve_skin(name: &str) -> &'static Theme;   // "dark" → &ANSI; unknown → &ANSI
pub fn skin_name(theme: &'static Theme) -> &'static str; // &ANSI → "ansi"
pub fn is_known_skin(name: &str) -> bool;              // true for "dark" too
```

Value mapping (old field → token), applied to all three palettes in this task:

| token | DARK → ANSI | LIGHT | MUR |
|---|---|---|---|
| `text` | `agent_text` `#eaeaea` | `#222233` | `#e0e0f0` |
| `muted` | `system` `#8a8a8a` | `system` `#777788` | `system` `#7777aa` |
| `emphasis` | `text` + BOLD | same | same |
| `accent` | `Cyan` | `#0e6b8c` | `#fbbf24` |
| `accent_alt` | `user` `Green` | `#166534` | `#a78bfa` |
| `ok` / `warn` / `error` | `#6cc07a` / `#e5a53a` / `#e06c6c` | `#1c7a3a` / `#b57400` / `#c03030` | `#80d090` / `#f0c060` / `#f08090` |
| `border` | `#555555` | `#d0d0e0` | `#505090` |
| `surface` | bg `#1e1e1e` (`card_bg`) | bg `#f1f1f7` | bg `#14142c` |
| `badge` | fg `Black` bg `Cyan` | fg `#0e6b8c` bg `#e0f0f8` | fg `#fbbf24` bg `#221a06` |

Values that move by construction (see Deviation): `DARK.user_text`
`#b8b8b8` → `text`+DIM; `DARK.border_title` `#707070` → `muted`;
`LIGHT.thinking` `#888899` → `muted`; `LIGHT.border_title` `#999999` → `muted`;
`MUR.thinking` `#8686c0` → `muted`; `MUR.border_title` `#555599` → `muted`;
`LIGHT.user_text`/`MUR.user_text` → `text`+DIM. `status_bg` is unused today
and goes; `DARK.status_bg = Reset` becomes `surface` bg for the status bar,
which is a visible change on `ansi` only if the status bar paints `surface` —
Task 2 keeps the status bar on `Style::default()` in PR-1, so it does not.

- [x] Replace `theme.rs` lines 1–113 (everything above `const KNOWN`) with:

```rust
//! Skin/theme definitions for the agent CLI TUI: one semantic token
//! vocabulary, three palettes. Tokens are `Style`, not `Color`, because
//! `ansi` says "muted" with `DIM` where `light` says it with a grey, and a
//! paint site must not know which. See
//! `docs/superpowers/specs/2026-09-09-murmur-skin-redesign-design.md`.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

pub struct Theme {
    // ── text ──────────────────────────────────────────────────────────────
    /// Body text: agent replies; user turns take this plus DIM.
    pub text: Style,
    /// Metadata: notices, thinking, hints, rule titles, timestamps.
    pub muted: Style,
    /// Headings and focused items.
    pub emphasis: Style,
    // ── identity ──────────────────────────────────────────────────────────
    /// "● agent" label, mascot, brand, focused-panel borders.
    pub accent: Style,
    /// "you ›" label, "$ cmd" shell label.
    pub accent_alt: Style,
    // ── status ────────────────────────────────────────────────────────────
    pub ok: Style,
    pub warn: Style,
    pub error: Style,
    // ── chrome ────────────────────────────────────────────────────────────
    /// Unfocused rules: composer top rule, table grid, fleet rail.
    pub border: Style,
    /// Status bar and card background (bg only).
    pub surface: Style,
    /// Agent-name / AUTO badge on the status bar.
    pub badge: Style,
    // ── layout ────────────────────────────────────────────────────────────
    pub border_type: BorderType,
    pub inner_padding: u8,
    pub compact_input: bool,
}

const fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

const fn rgb(r: u8, g: u8, b: u8) -> Style {
    Style::new().fg(Color::Rgb(r, g, b))
}

/// Follows the terminal: ANSI slots only, so the user's own theme decides
/// every colour. Alias `dark`.
pub const ANSI: Theme = Theme {
    text: rgb(0xea, 0xea, 0xea),
    muted: rgb(0x8a, 0x8a, 0x8a),
    emphasis: rgb(0xea, 0xea, 0xea).add_modifier(Modifier::BOLD),
    accent: fg(Color::Cyan),
    accent_alt: fg(Color::Green),
    ok: rgb(0x6c, 0xc0, 0x7a),
    warn: rgb(0xe5, 0xa5, 0x3a),
    error: rgb(0xe0, 0x6c, 0x6c),
    border: rgb(0x55, 0x55, 0x55),
    surface: Style::new().bg(Color::Rgb(0x1e, 0x1e, 0x1e)),
    badge: Style::new().fg(Color::Black).bg(Color::Cyan),
    border_type: BorderType::Plain,
    inner_padding: 1,
    compact_input: false,
};

pub const LIGHT: Theme = Theme {
    text: rgb(0x22, 0x22, 0x33),
    muted: rgb(0x77, 0x77, 0x88),
    emphasis: rgb(0x22, 0x22, 0x33).add_modifier(Modifier::BOLD),
    accent: rgb(0x0e, 0x6b, 0x8c),
    accent_alt: rgb(0x16, 0x65, 0x34),
    ok: rgb(0x1c, 0x7a, 0x3a),
    warn: rgb(0xb5, 0x74, 0x00),
    error: rgb(0xc0, 0x30, 0x30),
    border: rgb(0xd0, 0xd0, 0xe0),
    surface: Style::new().bg(Color::Rgb(0xf1, 0xf1, 0xf7)),
    badge: Style::new()
        .fg(Color::Rgb(0x0e, 0x6b, 0x8c))
        .bg(Color::Rgb(0xe0, 0xf0, 0xf8)),
    border_type: BorderType::Rounded,
    inner_padding: 1,
    compact_input: false,
};

pub const MUR: Theme = Theme {
    text: rgb(0xe0, 0xe0, 0xf0),
    muted: rgb(0x77, 0x77, 0xaa),
    emphasis: rgb(0xe0, 0xe0, 0xf0).add_modifier(Modifier::BOLD),
    accent: rgb(0xfb, 0xbf, 0x24),
    accent_alt: rgb(0xa7, 0x8b, 0xfa),
    ok: rgb(0x80, 0xd0, 0x90),
    warn: rgb(0xf0, 0xc0, 0x60),
    error: rgb(0xf0, 0x80, 0x90),
    border: rgb(0x50, 0x50, 0x90),
    surface: Style::new().bg(Color::Rgb(0x14, 0x14, 0x2c)),
    badge: Style::new()
        .fg(Color::Rgb(0xfb, 0xbf, 0x24))
        .bg(Color::Rgb(0x22, 0x1a, 0x06)),
    border_type: BorderType::Rounded,
    inner_padding: 1,
    compact_input: true,
};

/// The names `/skin` and `--skin` accept, in the order they are listed.
/// `dark` is an alias of `ansi` and is not listed.
pub const SKIN_NAMES: &str = "ansi, light, mur";

const KNOWN: [(&str, &Theme); 4] = [
    ("ansi", &ANSI),
    ("dark", &ANSI),
    ("light", &LIGHT),
    ("mur", &MUR),
];
```

  (PR-1 keeps `ANSI`'s greys as RGB on purpose — today's `DARK` values. PR-2
  Task 5 replaces them with `Reset` + modifiers; that is where the
  "no Rgb in ansi" guard lands.)

- [x] Replace `resolve_skin` and `skin_name` (old lines 116–131) with:

```rust
/// Resolve a skin name to a theme. `dark` is an alias of `ansi`; unknown
/// names fall back to `&ANSI`, the default.
pub fn resolve_skin(name: &str) -> &'static Theme {
    KNOWN
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| *t)
        .unwrap_or(&ANSI)
}

/// Canonical name of a theme instance — `"ansi"` for the alias too.
pub fn skin_name(theme: &'static Theme) -> &'static str {
    KNOWN
        .iter()
        .find(|(_, t)| std::ptr::eq(*t, theme))
        .map(|(n, _)| *n)
        .unwrap_or("ansi")
}
```

  `is_known_skin` stays as it is (the alias is in `KNOWN`, so `"dark"` is
  known). Delete `#![allow(dead_code)]` at the top — every token is used by
  the end of Task 3; if clippy reports one unused, that is a missed site,
  not a reason to restore the allow.

- [x] Replace the existing `mod tests` in `theme.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_is_an_alias_of_ansi() {
        assert!(std::ptr::eq(resolve_skin("dark"), &ANSI));
        assert!(std::ptr::eq(resolve_skin("ansi"), &ANSI));
        assert_eq!(skin_name(&ANSI), "ansi");
        assert!(is_known_skin("dark"), "saved config still says dark");
    }

    #[test]
    fn unknown_skins_fall_back_to_ansi() {
        assert!(std::ptr::eq(resolve_skin("neon"), &ANSI));
        assert!(!is_known_skin("neon"));
    }

    #[test]
    fn light_and_mur_resolve_to_themselves() {
        assert!(std::ptr::eq(resolve_skin("light"), &LIGHT));
        assert!(std::ptr::eq(resolve_skin("mur"), &MUR));
        assert_eq!(skin_name(&LIGHT), "light");
        assert_eq!(skin_name(&MUR), "mur");
    }
}
```

- [x] Run `cargo nextest run -p mur-core --lib cli::theme` — expected: the
  three tests pass; the crate does **not** compile yet elsewhere (`theme.agent`
  etc. no longer exist). That is Task 2's job; do not commit between Task 1
  and Task 3.

## Task 2 — paint sites in `ui/` take tokens

**Interfaces — Consumes:** `Theme` tokens from Task 1.
**Produces:** nothing new; every site below compiles against the token struct.

Rules for every replacement in this task and the next:

- `Style::default().fg(theme.X)` → `theme.T` (the token, a `Style`).
- `Style::default().fg(theme.X).add_modifier(M)` → `theme.T.add_modifier(M)`.
- A `Color` bound to a variable and styled later (`(msg, theme.agent)` then
  `Style::default().fg(color)`) → bind the `Style` and use it directly.
- `.bg(theme.card_bg)` → `.patch(theme.surface)`.
- A `Color` that a type requires (`MascotMode::Accent(Color)`) →
  `theme.T.fg.unwrap_or(Color::Reset)`.

- [x] `ui/message.rs` — apply, line by line (line numbers from `main` at
  `5e7242d3`):

| line | old | new |
|---|---|---|
| 59–63 | `if theme.show_separator && … { Line::styled("─".repeat(SEPARATOR_WIDTH), Style::default().fg(theme.separator)) }` | keep the shape but read the rule from the palette: PR-1 replaces `theme.show_separator` with `matches!(theme.border_type, BorderType::Rounded)` and `Style::default().fg(theme.separator)` with `theme.border` — `LIGHT` and `MUR` are the two `Rounded` skins, so this reproduces today's rule exactly; PR-2 deletes the branch. Add `use ratatui::widgets::BorderType;`. |
| 81 | `Style::default().fg(theme.agent)` | `theme.accent` |
| 86 | `.fg(theme.agent)` (inside a `Style::default()…` chain) | drop the chain, use `theme.accent.add_modifier(Modifier::BOLD)` |
| 96 | `.fg(theme.thinking)` | `theme.muted.add_modifier(Modifier::ITALIC)` (keep whatever modifiers the chain had) |
| 127, 131 | `Style::default().fg(theme.agent)` | `theme.accent` |
| 169 | `Style::default().fg(theme.user).add_modifier(Modifier::BOLD)` | `theme.accent_alt.add_modifier(Modifier::BOLD)` |
| 174 | `Style::default().fg(theme.user_text)` | `theme.text.add_modifier(Modifier::DIM)` |
| 182–185 | `(theme.system, "·")` … `(theme.success, "✔")` | `(theme.muted, "·")`, `(theme.warn, "▲")`, `(theme.error, "✖")`, `(theme.ok, "✔")` — and the binding two lines below that does `Style::default().fg(color)` becomes `color` (rename the tuple's first element to `style`) |
| 208 | `.fg(theme.shell)` | `theme.accent_alt` (same chain rule) |
| 215 | `Style::default().fg(theme.system)` | `theme.muted` |

- [x] `ui/band.rs`: line 392 `.fg(theme.agent)` chain → `theme.accent.add_modifier(Modifier::BOLD)`; line 521 `Style::default().fg(theme.border_title)` → `theme.muted`.
- [x] `ui/chooser.rs`: 82/94 `Style::default().fg(theme.system)` → `theme.muted`; 83/95 `Style::default().fg(theme.agent_text)` → `theme.text`; 111 `.border_style(Style::default().fg(theme.border))` → `.border_style(theme.border)`; 114 `.title_style(Style::default().fg(theme.border_title))` → `.title_style(theme.muted)`; 124 `.fg(theme.accent)` chain → `theme.accent.add_modifier(Modifier::BOLD)`.
- [x] `ui/rail.rs`: 54 `.fg(theme.border_title)` chain → `theme.muted` (keep the chain's modifiers); 61 `theme.warn` stays (it is now a `Style`; change the tuple type and the later `Style::default().fg(...)` to use it directly); 67 `theme.agent` → `theme.accent`; 69 `theme.success` → `theme.ok`; 70 `theme.error` stays; 82 `Style::default().fg(theme.system)` → `theme.muted`.
- [x] `ui/status.rs`: 48 `(msg, theme.agent)` → `(msg, theme.accent)`; 55 `(format!("ready{ctx}"), theme.system)` → `(…, theme.muted)`; the consumer of that tuple (`Style::default().fg(color)`) uses the style directly; 60 `Style::default().fg(theme.badge_fg).bg(theme.badge_bg)` → `theme.badge`; 121 `Style::default().fg(theme.agent)` → `theme.accent`; 156/161/168/176/178 `theme.system` → `theme.muted` (same tuple rule where applicable).
- [x] `ui.rs`: 177 `theme.system` → `theme.muted`; 178 `theme.agent_text` → `theme.text`; 191 `.fg(theme.border_title)` chain → `theme.muted`; 230 `.border_style(Style::default().fg(theme.border))` → `.border_style(theme.border)`; 233 `.title_style(…theme.border_title)` → `.title_style(theme.muted)`; 244 `.fg(theme.accent)` chain → `theme.accent.add_modifier(Modifier::BOLD)`.
- [x] `ui/band/tests.rs`: the `MUR` import and `app.theme = &MUR` stay valid; the `const { assert!(MUR.show_separator, …) }` in `consecutive_notices_draw_no_rule_and_turns_still_do` becomes `const { assert!(matches!(MUR.border_type, BorderType::Rounded), "test needs a ruled skin") }` with the `BorderType` import.
- [x] `ui/message.rs` tests: `gap_row(&MUR, …)` expectations are unchanged in PR-1 (the rule still paints for `MUR`).
- [x] Run `cargo check -p mur-core --lib` — expected: errors only in `render_card.rs`, `settlement.rs`, `diff.rs`, `welcome.rs`, `app.rs`, `mod.rs` (Task 3 and 4 files). Any error in a `ui/` file means a missed site above.

## Task 3 — paint sites outside `ui/`

**Interfaces — Consumes:** Task 1 tokens. **Produces:** `welcome::resolve_mascot_mode` unchanged in signature.

- [x] `render_card.rs`: 36 `_ => theme.agent` → `_ => theme.accent` (this `match` yields a `Color` today; make it yield a `Style` and update the two arms above it the same way — they are `theme.success`/`theme.error` → `theme.ok`/`theme.error`); every `Style::default().fg(theme.system)` (59, 79, 83, 95, 101, 112, 119, 140, 147, 178, 212, 219, 226) → `theme.muted`, keeping any `.add_modifier` the chain had; 163 `theme.agent_text` → `theme.text`.
- [x] `settlement.rs`: 50–55 becomes

```rust
    let style = match glyph {
        '✔' => theme.ok,
        '✘' => theme.error,
        '⚠' => theme.warn,
        _ => theme.text,
    };
    style.patch(theme.surface)
```

  and 132–133 `.fg(theme.border_title).bg(theme.card_bg)` → `theme.muted.patch(theme.surface)`; the doc comment at 114 says `theme.surface`.
- [x] `diff.rs`: 61, 90, 116 `theme.system` → `theme.muted` (chain/tuple rules).
- [x] `welcome.rs`: 146 `MascotMode::Accent(theme.accent)` → `MascotMode::Accent(theme.accent.fg.unwrap_or(Color::Reset))`; 203 `Style::default().fg(theme.system)` → `theme.muted`; 233 `.fg(theme.agent)` chain → `theme.accent.add_modifier(Modifier::BOLD)`; 236 `Style::default().fg(theme.separator)` → `theme.muted`; 237/243/255/264 `theme.system` → `theme.muted`; 247 `.fg(theme.user_text)` chain → `theme.text.add_modifier(Modifier::ITALIC)` (the chain had ITALIC); 259 `.fg(theme.agent)` chain → `theme.accent.add_modifier(Modifier::BOLD)`.
- [x] `app.rs` `sync_input_block` (1588–1626): line 1617 `.border_style(Style::default().fg(theme.border))` → `.border_style(theme.border)`; 1620 `.title_style(Style::default().fg(theme.border_title))` → `.title_style(theme.muted)`. The shell-mode red border stays `Style::default().fg(Color::Red)` — it is a mode signal, not a skin token; PR-2 changes it to `theme.error`.
- [x] `mod.rs`: 2249 and 2253 replace the literal `dark, light, mur` with `{}` and `theme::SKIN_NAMES`; 441 (`unknown skin '{skin_name}', using dark — valid: dark, light, mur`) → `unknown skin '{skin_name}', using ansi — valid: {}` with `theme::SKIN_NAMES`.
- [x] `mur-core/src/cli/agent.rs` 193: `/// Visual skin: ansi (default; `dark` is an alias) | light | mur`.
- [x] `mur-common/src/config.rs` 580–581: `/// Valid values: "ansi" (default; "dark" is an alias), "light", "mur".`
- [x] Run `cargo clippy -p mur-core --all-targets -- -D warnings` — expected clean. If it reports `unused import: Color` in a file, remove the import; if it reports a dead token in `theme.rs`, find the site.
- [x] Run `cargo nextest run -p mur-core --lib cmd::agent::cli::` — expected: all pass except tests that assert an old value of a merged role. Fix those by asserting the token (`theme.muted`) rather than a hex; do not widen any assertion.

## Task 4 — pin what PR-1 must not move; commit; PR

**Interfaces — Consumes:** everything above. **Produces:** `band_growth_tests::agent_turn_notice_and_status_bar_paint_the_same_cells_under_each_skin` (deleted in PR-2 Task 5).

- [x] Add to `ui/band/tests.rs`, inside `mod band_growth_tests`:

```rust
    /// PR-1 of the skin redesign is structure only: for the cells whose
    /// colour maps 1:1 (agent label and body, notices, status bar) the
    /// buffer under each skin is identical to the one recorded here from
    /// the pre-refactor build. Recorded, not derived: the point is to
    /// notice a paint site that took the wrong token.
    #[test]
    fn agent_turn_notice_and_status_bar_paint_the_same_cells_under_each_skin() {
        use crate::cmd::agent::cli::theme::{ANSI, LIGHT, MUR};
        for (name, theme) in [("ansi", &ANSI), ("light", &LIGHT), ("mur", &MUR)] {
            let mut app = App::test_fixture();
            app.theme = theme;
            app.messages.push(ChatMsg::for_test(Role::Agent, "hello **there**"));
            app.push_system("skin changed");
            let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
            term.draw(|f| render(f, &mut app)).unwrap();
            let buf = term.backend().buffer().clone();
            // Agent label cell and first body cell: token `accent` / `text`.
            let label = buf.cell((1, 0)).unwrap();
            let body = buf.cell((3, 1)).unwrap();
            let notice = buf.cell((1, 3)).unwrap();
            assert_eq!(label.fg, theme.accent.fg.unwrap(), "{name}: agent label");
            assert_eq!(body.fg, theme.text.fg.unwrap(), "{name}: agent body");
            assert_eq!(notice.fg, theme.muted.fg.unwrap(), "{name}: notice");
        }
    }
```

  Before running, print the buffer once (`eprintln!("{buf:?}")`) and adjust
  the three coordinates to the cells that actually hold `●`, `h` of `hello`,
  and `·` of the notice under the fixture's welcome header; then delete the
  `eprintln!`. Expected: passes under all three skins.
- [x] `cargo fmt -p mur-core`; `cargo clippy … -D warnings` clean; full
  `cargo nextest run -p mur-core --lib` green.
- [x] Live check (the installed binary is not this branch): `MUR_WEB_DIST=… ./build.sh --install`, then in tmux `murmur mur --skin dark`, `--skin ansi`, `--skin light`, `--skin mur`: `/skin` with no argument prints `current skin: ansi — valid: ansi, light, mur` for the first two; each transcript looks as it did before this branch (agent gold/cyan label, notices grey, rule between turns on light/mur).
- [x] Commit on `refactor/murmur-theme-tokens`:

```
refactor(murmur): Theme is eleven semantic tokens; `ansi` names the terminal-following skin, `dark` is its alias

Structure only. Every paint site takes a token (`text`, `muted`, `emphasis`,
`accent`, `accent_alt`, `ok`, `warn`, `error`, `border`, `surface`, `badge`)
instead of composing a colour and a modifier of its own; the three palettes
hold today's values. Where two old fields merged (`user_text`/`agent_text`,
`thinking`/`system`/`border_title`) the surviving value is the more
frequent role's — listed in the plan. Visual redesign follows in its own PR.
```

  Open the PR against `main` with the value-mapping table from Task 1 in the
  body. Merge on green.

---

# PR-2 — the redesign

Branch `feat/murmur-skin-redesign` from `main` after PR-1 merges.

## Task 5 — final palettes and the guards

**Interfaces — Consumes:** Task 1 struct. **Produces:** `theme::ASSUMED_BG_LIGHT`, `theme::ASSUMED_BG_MUR` (`Color::Rgb`), `theme::contrast_ratio(Color, Color) -> f64` (test-only, `#[cfg(test)]`).

- [ ] Write the failing guards first. Replace `mod tests` in `theme.rs` with the three tests from Task 1 plus:

```rust
    /// WCAG 2 relative luminance of an sRGB colour.
    fn luminance(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else {
            panic!("contrast is only defined for Rgb tokens, got {c:?}")
        };
        let lin = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    fn contrast_ratio(a: Color, b: Color) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// The truecolor skins assume a background and every text token must
    /// read against it: body 7:1, everything else 4.5:1. The assumed
    /// background is a constant beside the palette so this test and the
    /// spec cannot drift apart.
    #[test]
    fn light_and_mur_meet_wcag() {
        for (name, theme, bg) in [("light", &LIGHT, ASSUMED_BG_LIGHT), ("mur", &MUR, ASSUMED_BG_MUR)] {
            let fg = |s: Style| s.fg.expect("text token has a colour");
            assert!(contrast_ratio(fg(theme.text), bg) >= 7.0, "{name}: text");
            for (label, s) in [
                ("muted", theme.muted), ("emphasis", theme.emphasis),
                ("accent", theme.accent), ("accent_alt", theme.accent_alt),
                ("ok", theme.ok), ("warn", theme.warn), ("error", theme.error),
            ] {
                let r = contrast_ratio(fg(s), bg);
                assert!(r >= 4.5, "{name}: {label} is {r:.1}:1");
            }
            let badge_bg = theme.badge.bg.expect("badge has a background");
            let r = contrast_ratio(fg(theme.badge), badge_bg);
            assert!(r >= 4.5, "{name}: badge is {r:.1}:1");
        }
    }

    /// `ansi` pins nothing: every token is a named ANSI colour or Reset, so
    /// the terminal's own theme is what the user sees.
    #[test]
    fn ansi_pins_no_colour() {
        let named = |c: Option<Color>| match c {
            None | Some(Color::Reset) => true,
            Some(Color::Rgb(..)) | Some(Color::Indexed(_)) => false,
            Some(_) => true,
        };
        for (label, s) in [
            ("text", ANSI.text), ("muted", ANSI.muted), ("emphasis", ANSI.emphasis),
            ("accent", ANSI.accent), ("accent_alt", ANSI.accent_alt),
            ("ok", ANSI.ok), ("warn", ANSI.warn), ("error", ANSI.error),
            ("border", ANSI.border), ("surface", ANSI.surface), ("badge", ANSI.badge),
        ] {
            assert!(named(s.fg) && named(s.bg), "ansi.{label} pins a colour: {s:?}");
        }
    }
```

- [ ] Run `cargo nextest run -p mur-core --lib cli::theme` — expected:
  `light_and_mur_meet_wcag` fails on `mur: muted is 4.6:1` (or `light`
  first), `ansi_pins_no_colour` fails on `ansi.text`.
- [ ] Replace the three palettes with the spec's §2 values:

```rust
pub const ANSI: Theme = Theme {
    text: Style::new(),
    muted: Style::new().add_modifier(Modifier::DIM),
    emphasis: Style::new().add_modifier(Modifier::BOLD),
    accent: fg(Color::Cyan),
    accent_alt: fg(Color::Green),
    ok: fg(Color::Green),
    warn: fg(Color::Yellow),
    error: fg(Color::Red),
    border: Style::new().add_modifier(Modifier::DIM),
    surface: Style::new(),
    badge: fg(Color::Cyan).add_modifier(Modifier::REVERSED).add_modifier(Modifier::BOLD),
    border_type: BorderType::Plain,
    inner_padding: 1,
    compact_input: false,
};

/// Background `light` is designed against (it does not paint one).
pub const ASSUMED_BG_LIGHT: Color = Color::Rgb(0xff, 0xff, 0xff);

pub const LIGHT: Theme = Theme {
    text: rgb(0x1f, 0x24, 0x30),
    muted: rgb(0x5c, 0x63, 0x70),
    emphasis: rgb(0x00, 0x00, 0x00).add_modifier(Modifier::BOLD),
    accent: rgb(0x0b, 0x6e, 0x8f),
    accent_alt: rgb(0x1a, 0x6b, 0x3a),
    ok: rgb(0x1a, 0x7f, 0x3a),
    warn: rgb(0x8a, 0x5a, 0x00),
    error: rgb(0xb3, 0x26, 0x1e),
    border: rgb(0xc9, 0xcc, 0xd6),
    surface: Style::new().bg(Color::Rgb(0xee, 0xf0, 0xf5)),
    badge: Style::new()
        .fg(Color::Rgb(0x0b, 0x6e, 0x8f))
        .bg(Color::Rgb(0xe0, 0xf0, 0xf8)),
    border_type: BorderType::Rounded,
    inner_padding: 1,
    compact_input: false,
};

/// Background `mur` is designed against (it does not paint one).
pub const ASSUMED_BG_MUR: Color = Color::Rgb(0x0b, 0x0b, 0x1a);

pub const MUR: Theme = Theme {
    text: rgb(0xe4, 0xe4, 0xf4),
    muted: rgb(0x9a, 0x9a, 0xc4),
    emphasis: rgb(0xff, 0xff, 0xff).add_modifier(Modifier::BOLD),
    accent: rgb(0xfb, 0xbf, 0x24),
    accent_alt: rgb(0xb3, 0x9d, 0xfb),
    ok: rgb(0x7f, 0xd4, 0x8f),
    warn: rgb(0xf2, 0xc7, 0x6a),
    error: rgb(0xf2, 0x8b, 0x98),
    border: rgb(0x3f, 0x3f, 0x78),
    surface: Style::new().bg(Color::Rgb(0x14, 0x14, 0x2c)),
    badge: Style::new()
        .fg(Color::Rgb(0xfb, 0xbf, 0x24))
        .bg(Color::Rgb(0x22, 0x1a, 0x06)),
    border_type: BorderType::Rounded,
    inner_padding: 1,
    compact_input: true,
};
```

- [ ] Delete `agent_turn_notice_and_status_bar_paint_the_same_cells_under_each_skin` from `ui/band/tests.rs` (its `theme.text.fg.unwrap()` panics on `ansi` now, and its job is done).
- [ ] Run the theme tests — expected: all five pass. `cargo clippy` clean.
- [ ] Commit: `feat(murmur): final skin palettes — ansi follows the terminal, light and mur meet WCAG`.

## Task 6 — layout: no inter-turn rule, one rule above the composer, accent on focused panels

**Interfaces — Consumes:** Task 5. **Produces:** `INPUT_H_MIN = 2`; `gap_row` returns `Line::default()` unconditionally; two tests in `ui/band/tests.rs`.

- [ ] Write the failing tests in `ui/band/tests.rs`, `mod band_growth_tests`:

```rust
    /// The role label already says the speaker changed; a rule under it was
    /// one more line on a screen full of them. No skin draws one.
    #[test]
    fn no_rule_between_turns_under_any_skin() {
        use crate::cmd::agent::cli::theme::{ANSI, LIGHT, MUR};
        for (name, theme) in [("ansi", &ANSI), ("light", &LIGHT), ("mur", &MUR)] {
            let mut app = App::test_fixture();
            app.theme = theme;
            app.flushed_upto = 0;
            app.messages.push(ChatMsg::for_test(Role::User, "hi"));
            app.messages.push(ChatMsg::for_test(Role::Agent, "hello"));
            let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
            term.draw(|f| render_transcript(f, &mut app, Rect::new(0, 0, 80, 30)))
                .unwrap();
            let d = term.backend().to_string();
            let ruled = d
                .lines()
                .filter(|l| l.chars().filter(|c| *c == '─').count() > 10)
                .count();
            assert_eq!(ruled, 0, "{name} drew a rule between turns:\n{d}");
        }
    }

    /// One rule above the input row, and the status bar directly under it —
    /// the composer's bottom border marked the same seam the status bar does.
    #[test]
    fn composer_has_one_rule_and_the_status_bar_sits_under_the_input() {
        let mut app = App::test_fixture();
        app.messages.push(ChatMsg::for_test(Role::User, "hi"));
        let mut term = Terminal::with_options(
            TestBackend::new(80, 20),
            TerminalOptions { viewport: Viewport::Inline(20) },
        )
        .unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let d = term.backend().to_string();
        let rows: Vec<&str> = d.lines().collect();
        // Bottom-up: status bar, input row, the composer's top rule.
        let status = rows[rows.len() - 1];
        let input = rows[rows.len() - 2];
        let rule = rows[rows.len() - 3];
        assert!(status.contains("ready"), "status bar not last:\n{d}");
        assert!(input.contains("Type a message"), "input row not above the status bar:\n{d}");
        assert!(rule.contains("message —"), "composer rule not above the input row:\n{d}");
        assert!(!input.contains('─'), "a rule between input and status bar:\n{d}");
    }
```

  (`TestBackend::to_string` wraps rows in quotes; if `rows[...]` does not line
  up, strip the quotes with `.trim_matches('"')` before matching.)
- [ ] Run them — expected: the first fails for `light` and `mur` (rule painted), the second fails on `!input.contains('─')` (bottom border present).
- [ ] `ui/message.rs`: `gap_row` becomes

```rust
/// The gap before a message: a blank line in every skin. The role label is
/// the change-of-speaker signal; a rule under it repeated the information
/// (spec decision 4). Kept as a function because `message_block` attributes
/// the gap to the message it precedes, and that is what keeps the measured
/// band and the painted band the same rows.
pub(super) fn gap_row(
    _theme: &'static crate::cmd::agent::cli::theme::Theme,
    _prev: Option<&crate::cmd::agent::cli::app::ChatMsg>,
    _m: &crate::cmd::agent::cli::app::ChatMsg,
) -> Line<'static> {
    Line::default()
}
```

  Delete `SEPARATOR_WIDTH` and its doc; delete the `BorderType` import added
  in Task 2. In `ui/message.rs` tests, `gap_tests` becomes one test asserting
  `gap_row(&MUR, Some(&u), &a)` and `gap_row(&ANSI, …)` are both empty. In
  `ui/band/tests.rs`, `consecutive_notices_draw_no_rule_and_turns_still_do`
  loses its last assertion (`text(3).contains('─')`) and its `const` guard,
  and is renamed `no_turn_draws_a_rule`.
- [ ] `app.rs` `sync_input_block`: both blocks `Borders::TOP | Borders::BOTTOM` → `Borders::TOP`; the shell block's `Style::default().fg(Color::Red)` → `theme.error`. `ui.rs`: `INPUT_H_MIN` 3 → 2 with its doc `/// Composer height when the input is empty (one text row plus its top rule).`; line 55 `(input_lines + 2)` → `(input_lines + 1)`. `INPUT_H_MAX` stays 8.
- [ ] Focused panels take `accent` on their border: `ui/chooser.rs` 111 `.border_style(theme.border)` → `.border_style(theme.accent)`; `ui.rs` 230 (completion popup) the same; `ui/hitl.rs` `.border_style(Style::default().fg(Color::Yellow))` → `.border_style(theme.accent)` — `render_hitl` does not take a theme today; add `theme: &'static Theme` as its first parameter and pass `app.theme` from `ui.rs` `render`, updating `hitl_modal_tests` to pass `&ANSI`.
- [ ] Run the two new tests and the whole `cmd::agent::cli::` suite — expected green. `chooser_floor_tests::the_chooser_leaves_the_transcript_more_than_three_rows` and `ctrl_up_still_reaches_the_spaced_form` compute with `input_height = 3`; they pass a literal 3 and are unaffected, but re-read them: if either asserts a value derived from `INPUT_H_MIN`, update the expected number by one.
- [ ] Live check in tmux under each skin: no rule between turns; exactly one rule above the input; status bar directly under; chooser and approval modal borders in the accent colour.
- [ ] Commit: `feat(murmur): no rule between turns, one rule above the composer, accent on focused panels`.

## Task 7 — default `ansi`, notices, docs

**Interfaces — Consumes:** Tasks 5–6. **Produces:** nothing new.

- [ ] Default: `resolve_skin` already falls back to `&ANSI`; find where the
  configured skin is read at startup (`mod.rs` around line 430: the
  `skin_name` / `unknown_skin` block) and confirm the absent-config path
  resolves `"ansi"`. If a literal `"dark"` is the fallback there, change it to
  `"ansi"`.
- [ ] `README.md`: find the sentence that lists the skins (grep `--skin`) and
  make it read: ``Three skins: `ansi` (default — follows your terminal's own
  colours), `light`, and `mur` (the brand skin, purple on gold). `/skin
  <name>` switches and remembers; `dark` still works as an alias of `ansi`.``
- [ ] Docs site: the `update-docs` skill names the mur-server paths; the skin
  section gets the same sentence. This is a separate repo and PR; note it in
  the PR body as a follow-up if it cannot land the same day.
- [ ] Full suite, clippy, fmt. Live check: `murmur mur` with no `--skin` and
  no `agent_cli.skin` in config shows the terminal's own colours (agent label
  in the terminal's cyan); `/skin` lists `ansi, light, mur`; `--skin dark`
  behaves as `ansi`.
- [ ] Commit: `feat(murmur): ansi is the default skin`. Open the PR with
  before/after screenshots of the three skins in the body.

---

## Self-review

- **Spec coverage:** decisions 1–3 → Task 1 (name, alias) and Task 7 (default); 4 → Task 6; 5 → Task 6; 6 → Task 5; 7 → Tasks 1–3; 8 → Task 5 (`mur` accent unchanged, accent_alt lightened, greys moved). §3 focused-panel accent → Task 6. §4 guards → Tasks 5–6. §5 notices/help/config → Tasks 3 and 7. §6 two PRs → the two halves.
- **Placeholders:** none; every replacement names the line, the old text and the new text. Task 4's three cell coordinates are the one thing the implementer must confirm against the printed buffer, and the step says how.
- **Cross-task names:** `ANSI`/`LIGHT`/`MUR`, `SKIN_NAMES`, `ASSUMED_BG_LIGHT`/`ASSUMED_BG_MUR`, `INPUT_H_MIN`, `gap_row`, `render_hitl(theme, …)` are used with the same spelling in every task that touches them.
