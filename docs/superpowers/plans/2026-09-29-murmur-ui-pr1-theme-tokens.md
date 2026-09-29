# murmur UI PR-1 — every colour through the theme

> **Execute with `mur-executing-plans`** (in-context, task by task, stop on a
> blocker). Tasks are sequential: each one's Interfaces block is what the
> next consumes.

**Goal:** Route every colour murmur paints through the skin's tokens, add
`surface_alt` and `badge_warn`, and guard every built-in skin with one
contrast check over the pairs it actually paints.

**Architecture:** `theme.rs` becomes `theme/` (palettes in `mod.rs`, guard
tests in `tests.rs`, a reusable `contrast.rs`). Markdown rendering, which
caches its output per message, starts taking the theme, so a skin switch
goes through one `App::apply_theme` that also re-renders the cache. Paint
sites that pinned `Color::*` switch to tokens.

**Tech stack:** Rust 2024, ratatui 0.29, tui-textarea 0.7, pulldown-cmark
0.12, cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-09-29-murmur-ui-p1p2-design.md` —
§1 (tokens, paint-site table), decisions 2–4, §6 contrast table, §7 guards
marked below. PR-2 (text rendering), PR-3 (approval panel) and PR-4 (user
skins) get their own plans after this merges.

## Global Constraints

Copied from the spec and CLAUDE.md; every task includes them.

- `ansi` paints no decorative background: no zebra stripe, no code-block fill, no diff tint, no card surface.
- Reverse video on `ansi` is allowed only on the status row and the one focused element.
- New tokens are exactly `surface_alt` (bg only) and `badge_warn`. `READS` and `MONITOR` use `badge`.
- User body text takes `muted` in every skin; RGB skins carry no `DIM` on any text token.
- Every foreground is measured against the background it is painted on (§6 contrast table); body text ≥ 7:1, other text ≥ 4.5:1, diff marks ≥ 3:1.
- `markdown.rs` `CODE` (`Yellow`), `welcome.rs`'s mascot RGB, and everything in `ui/hitl.rs` except its `DarkGray` sites stay as they are in this PR.
- No source file over 800 lines (CLAUDE.md rule 5). Pure moves are their own commit.
- No hardcoded values: colours live in `theme/mod.rs`, thresholds are named constants.
- Lint gate is CI's: `cargo clippy --all --all-targets --no-deps --locked -- -D warnings` and `cargo fmt --all -- --check`.
- Tests run under `cargo nextest`, never `cargo test` (shared-process env races in mur-core).

## Environment (every shell in this plan)

```bash
export MUR_WEB_DIST="$HOME/Projects/mur-web/dist"   # mur-core embeds it; without it: E0599 WebAssets::get
export ORT_STRATEGY=download                         # onnxruntime link
export RUST_MIN_STACK=33554432                       # mur-core test binaries overflow the default stack
```

All paths below are relative to the code worktree root
`/Volumes/Firecuda4tb/Projects/mur/.worktrees/murmur-theme-tokens`, and
`CLI` means `mur-core/src/cmd/agent/cli`.

## File structure

| file | change | responsibility |
|---|---|---|
| `CLI/theme.rs` → `CLI/theme/mod.rs` | move, then edit | `Theme`, the four palettes, name resolution |
| `CLI/theme/tests.rs` | new (moved) | palette guard tests |
| `CLI/theme/contrast.rs` | new | WCAG ratio, DIM model, the pair table, `check()` |
| `CLI/markdown.rs` | edit | render takes the theme; stripe/quote/rule/grid/marker/header use tokens |
| `CLI/markdown/theme_tests.rs` | new | markdown paints only with the skin |
| `CLI/app/msg.rs` | edit | `ChatMsg::agent_rendered` takes the theme |
| `CLI/app/mod.rs` | edit | `new_input(theme)`; restore passes the theme |
| `CLI/app/transcript.rs` | edit | render calls pass the theme |
| `CLI/app/usage.rs` | edit | `rerender_markdown` passes the theme; new `apply_theme` |
| `CLI/app/tests/theme_tests.rs` | new | `apply_theme` re-stripes and restyles |
| `CLI/app/tests/mod.rs` | edit | register `theme_tests` |
| `CLI/slash_cmds.rs` | edit | `/skin` calls `apply_theme` |
| `CLI/ui/message.rs` | edit | render call passes the theme; user body `muted` |
| `CLI/ui/status.rs` | edit | chips `badge`/`badge_warn`; countdown `warn`; issue `error` |
| `CLI/ui/chooser.rs` | edit | `DarkGray` → `muted` |
| `CLI/ui.rs` | edit | `DarkGray` → `muted`; register `theme_paint_tests` |
| `CLI/ui/hitl.rs` | edit | `DarkGray` → `muted` only |
| `CLI/ui/theme_paint_tests.rs` | new | status-row and message paint guards |
| `CLI/render_card.rs` | edit | error accent and error line use `error` |

---

## Task 0: Code worktree

**Interfaces** — Produces: worktree `.worktrees/murmur-theme-tokens` on
branch `feat/murmur-theme-tokens`, based on `origin/main`.

- [ ] Create it (run from the main checkout):

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git fetch origin main
git worktree add -b feat/murmur-theme-tokens .worktrees/murmur-theme-tokens origin/main
cd .worktrees/murmur-theme-tokens
git log --oneline -1
```

Expected: one line, the current `origin/main` head.

- [ ] Baseline — the theme tests pass before anything changes:

```bash
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::theme::/)'
```

Expected: `… passed, 0 failed`.

---

## Task 1: Move theme tests out (pure move)

**Interfaces** — Consumes: Task 0. Produces: module `theme` as a directory
(`theme/mod.rs`, `theme/tests.rs`); every public item keeps its path
`crate::cmd::agent::cli::theme::*`.

- [ ] Move the file and split its test module out verbatim:

```bash
cd mur-core/src/cmd/agent/cli
mkdir theme
git mv theme.rs theme/mod.rs
python3 - <<'EOF'
from pathlib import Path
p = Path("theme/mod.rs")
s = p.read_text()
marker = "#[cfg(test)]\nmod tests {\n"
i = s.index(marker)
head, body = s[:i], s[i + len(marker):]
assert body.rstrip().endswith("}"), "test module must close the file"
body = body.rstrip()[:-1]  # drop the module's closing brace
lines = [l[4:] if l.startswith("    ") else l for l in body.split("\n")]
Path("theme/tests.rs").write_text(
    "//! Palette guards, moved out of `theme.rs` for CLAUDE.md's 800-line rule.\n"
    "//! Pure movement: dedented one level, nothing else.\n\n"
    + "\n".join(lines).strip("\n") + "\n"
)
p.write_text(head + "#[cfg(test)]\nmod tests;\n")
EOF
cd -
```

- [ ] Verify nothing but location changed:

```bash
cargo fmt --all
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::theme::/)'
git diff --stat -M HEAD
```

Expected: the same pass count as Task 0's baseline; the diff stat shows
`theme.rs => theme/mod.rs` and a new `theme/tests.rs`, nothing else.

- [ ] Commit:

```bash
git add -A mur-core/src/cmd/agent/cli/theme mur-core/src/cmd/agent/cli/theme.rs
git commit -m "refactor(murmur): move theme tests into theme/tests.rs (pure move)"
```

---

## Task 2: `surface_alt`, `badge_warn`, and the palette guards

**Interfaces** — Consumes: Task 1. Produces:
`Theme: Clone + Copy + Debug`; fields `pub surface_alt: Style` and
`pub badge_warn: Style` on every palette (values in the spec §1 table).

- [ ] Write the failing guards. Append to `CLI/theme/tests.rs`:

```rust
/// `ansi` never learns the terminal's background, so it paints none: no
/// stripe, no card surface, no diff tint. The only background it may show is
/// reverse video on a status chip or the one focused row.
#[test]
fn ansi_paints_no_decorative_bg() {
    for (label, s) in [
        ("surface", ANSI.surface),
        ("surface_alt", ANSI.surface_alt),
        ("settlement_surface", ANSI.settlement_surface),
        ("diff_add_bg", ANSI.diff_add_bg),
        ("diff_del_bg", ANSI.diff_del_bg),
    ] {
        assert!(s.bg.is_none(), "ansi.{label} paints a background: {s:?}");
    }
}

/// Every token that inks text, by name — the set the no-DIM and contrast
/// guards walk.
fn ink_tokens(t: &Theme) -> [(&'static str, Style); 21] {
    [
        ("text", t.text),
        ("muted", t.muted),
        ("emphasis", t.emphasis),
        ("accent", t.accent),
        ("accent_alt", t.accent_alt),
        ("ok", t.ok),
        ("warn", t.warn),
        ("error", t.error),
        ("diff_add_mark", t.diff_add_mark),
        ("diff_del_mark", t.diff_del_mark),
        ("diff_add_text", t.diff_add_text),
        ("diff_del_text", t.diff_del_text),
        ("settlement_text", t.settlement_text),
        ("settlement_muted", t.settlement_muted),
        ("settlement_accent", t.settlement_accent),
        ("settlement_ok", t.settlement_ok),
        ("settlement_warn", t.settlement_warn),
        ("settlement_error", t.settlement_error),
        ("badge", t.badge),
        ("badge_warn", t.badge_warn),
        ("border", t.border),
    ]
}

/// An RGB skin says "quieter" with a measured colour. DIM hands that choice
/// to the terminal, which on a light background lands far under 4.5:1 —
/// how user turns became unreadable on `light`.
#[test]
fn rgb_skins_carry_no_dim() {
    for (name, t) in [("light", &LIGHT), ("mur", &MUR), ("clay", &CLAY)] {
        for (label, s) in ink_tokens(t) {
            assert!(
                !s.add_modifier.contains(Modifier::DIM),
                "{name}.{label} carries DIM: {s:?}"
            );
        }
    }
}

/// The two new tokens exist on every skin with the shape their paint sites
/// rely on: a stripe is background only; a warn chip has ink on a ground.
#[test]
fn surface_alt_is_bg_only_and_badge_warn_is_a_chip() {
    for (name, t) in [("ansi", &ANSI), ("light", &LIGHT), ("mur", &MUR), ("clay", &CLAY)] {
        assert!(t.surface_alt.fg.is_none(), "{name}.surface_alt sets a foreground");
        assert!(t.badge_warn.fg.is_some(), "{name}.badge_warn has no foreground");
    }
    for (name, t) in [("light", &LIGHT), ("mur", &MUR), ("clay", &CLAY)] {
        assert!(t.surface_alt.bg.is_some(), "{name} does not stripe");
        assert!(t.badge_warn.bg.is_some(), "{name}.badge_warn has no ground");
    }
    assert!(
        ANSI.badge_warn.add_modifier.contains(Modifier::REVERSED),
        "ansi chips are reverse video"
    );
}
```

Also extend the list in the existing `ansi_pins_no_colour` test: after the
line `("diff_del_text", ANSI.diff_del_text),` add

```rust
            ("surface_alt", ANSI.surface_alt),
            ("badge_warn", ANSI.badge_warn),
            ("settlement_surface", ANSI.settlement_surface),
```

- [ ] Watch it fail:

```bash
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::theme::/)'
```

Expected: compile error `no field 'surface_alt' on type 'Theme'`.

- [ ] Add the fields. In `CLI/theme/mod.rs` replace

```rust
pub struct Theme {
```

with

```rust
#[derive(Clone, Copy, Debug)]
pub struct Theme {
```

replace

```rust
    /// Status bar and ordinary card background (bg only).
    pub surface: Style,
```

with

```rust
    /// Status bar and ordinary card background (bg only).
    pub surface: Style,
    /// Alternate-row background: the table zebra stripe (bg only). `ansi`
    /// leaves it empty — it never learns the terminal's background, and a
    /// guessed slab is what hid striped rows on light terminals.
    pub surface_alt: Style,
```

and replace

```rust
    /// Agent-name / AUTO badge on the status bar; the SETTLEMENT title chip.
    pub badge: Style,
```

with

```rust
    /// Agent-name / READS / MONITOR chip on the status bar; the SETTLEMENT
    /// title chip.
    pub badge: Style,
    /// A status chip that means risk is on: AUTO and AUTO:<tools>.
    pub badge_warn: Style,
```

- [ ] Fill the four palettes. In `ANSI` replace

```rust
    border: Style::new().add_modifier(Modifier::DIM),
    surface: Style::new(),
```

with

```rust
    border: Style::new().add_modifier(Modifier::DIM),
    surface: Style::new(),
    surface_alt: Style::new(),
```

and replace

```rust
    badge: fg(Color::Cyan)
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD),
    border_type: BorderType::Plain,
```

with

```rust
    badge: fg(Color::Cyan)
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD),
    badge_warn: fg(Color::Yellow)
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD),
    border_type: BorderType::Plain,
```

In `LIGHT` replace

```rust
    surface: Style::new().bg(Color::Rgb(0xee, 0xf0, 0xf5)),
```

with

```rust
    surface: Style::new().bg(Color::Rgb(0xee, 0xf0, 0xf5)),
    surface_alt: Style::new().bg(Color::Rgb(0xe9, 0xec, 0xf2)),
```

and replace

```rust
        .bg(Color::Rgb(0xe0, 0xf0, 0xf8)),
```

with

```rust
        .bg(Color::Rgb(0xe0, 0xf0, 0xf8)),
    badge_warn: Style::new()
        .fg(Color::Rgb(0x8a, 0x5a, 0x00))
        .bg(Color::Rgb(0xfb, 0xef, 0xd5)),
```

In `MUR` replace

```rust
    surface: Style::new().bg(Color::Rgb(0x14, 0x14, 0x2c)),
```

with

```rust
    surface: Style::new().bg(Color::Rgb(0x14, 0x14, 0x2c)),
    surface_alt: Style::new().bg(Color::Rgb(0x1a, 0x1a, 0x36)),
```

and replace

```rust
        .bg(Color::Rgb(0x22, 0x1a, 0x06)),
```

with

```rust
        .bg(Color::Rgb(0x22, 0x1a, 0x06)),
    badge_warn: Style::new()
        .fg(Color::Rgb(0x0b, 0x0b, 0x1a))
        .bg(Color::Rgb(0xf2, 0xc7, 0x6a)),
```

In `CLAY` replace

```rust
    surface: Style::new().bg(Color::Rgb(0x26, 0x26, 0x26)),
```

with

```rust
    surface: Style::new().bg(Color::Rgb(0x26, 0x26, 0x26)),
    surface_alt: Style::new().bg(Color::Rgb(0x2a, 0x2a, 0x2a)),
```

and replace

```rust
        .bg(Color::Rgb(0xd9, 0x77, 0x57)),
```

with

```rust
        .bg(Color::Rgb(0xd9, 0x77, 0x57)),
    badge_warn: Style::new()
        .fg(Color::Rgb(0x1a, 0x1a, 0x1a))
        .bg(Color::Rgb(0xff, 0xc1, 0x07)),
```

- [ ] Watch it pass:

```bash
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::theme::/)'
```

Expected: all pass, including the three new tests.

- [ ] Commit:

```bash
git add mur-core/src/cmd/agent/cli/theme
git commit -m "feat(murmur): surface_alt and badge_warn tokens, no-DIM and no-decorative-bg guards"
```

---

## Task 3: One contrast check over the pairs a skin paints

**Interfaces** — Consumes: Task 2 (`Theme: Copy`, the two new fields).
Produces, in `crate::cmd::agent::cli::theme::contrast`:

```rust
pub const MIN_BODY: f64;   // 7.0
pub const MIN_TEXT: f64;   // 4.5
pub const MIN_GLYPH: f64;  // 3.0
#[derive(Debug, Clone, PartialEq)]
pub struct Failure { pub fg: &'static str, pub on: &'static str, pub ratio: f64, pub min: f64 }
impl std::fmt::Display for Failure  // "warn 2.1:1 on assumed_bg (needs 4.5:1)"
pub fn ratio(a: Color, b: Color) -> Option<f64>;
pub fn check(theme: &Theme, assumed_bg: Color) -> Vec<Failure>;
```

PR-4 (user skins) calls `check` at load; keep the signature.

- [ ] Create `CLI/theme/contrast.rs` with the types, a stub `check`, and
its tests:

```rust
//! WCAG 2 contrast over the colour pairs a skin actually paints. A token is
//! measured against the background it sits on — its own `bg`, else the
//! surface it is drawn over, else the skin's assumed terminal background —
//! so a diff tint or a table stripe that swallows its text is caught, not
//! only a token that vanishes into the terminal. Built-in skins are held to
//! it by `rgb_skins_meet_wcag`; user skin files are checked with the same
//! function when they load.

use super::Theme;
use ratatui::style::{Color, Modifier, Style};

/// Body text.
pub const MIN_BODY: f64 = 7.0;
/// Every other text token.
pub const MIN_TEXT: f64 = 4.5;
/// A glyph that is not read as text — the diff gutter marks (WCAG 1.4.11).
pub const MIN_GLYPH: f64 = 3.0;

/// One painted pair that falls short.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub fg: &'static str,
    pub on: &'static str,
    pub ratio: f64,
    pub min: f64,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {:.1}:1 on {} (needs {}:1)",
            self.fg, self.ratio, self.on, self.min
        )
    }
}

/// WCAG 2 contrast ratio of two colours; `None` unless both are `Rgb` — a
/// named slot or the terminal default has no colour murmur can know.
pub fn ratio(a: Color, b: Color) -> Option<f64> {
    Some(ratio_rgb(rgb(Some(a))?, rgb(Some(b))?))
}

pub fn check(_theme: &Theme, _assumed_bg: Color) -> Vec<Failure> {
    Vec::new()
}

fn rgb(c: Option<Color>) -> Option<[u8; 3]> {
    match c {
        Some(Color::Rgb(r, g, b)) => Some([r, g, b]),
        _ => None,
    }
}

fn luminance(c: [u8; 3]) -> f64 {
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

fn ratio_rgb(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::agent::cli::theme::{ASSUMED_BG_LIGHT, ASSUMED_BG_MUR, LIGHT, MUR};

    fn pairs(f: &[Failure]) -> Vec<(&'static str, &'static str)> {
        f.iter().map(|x| (x.fg, x.on)).collect()
    }

    const INK: Color = Color::Rgb(0xe4, 0xe4, 0xf4); // MUR.text

    /// The review's case: a tint equal to the diff text passes every
    /// token-against-terminal check and hides the addition entirely.
    #[test]
    fn a_diff_tint_that_swallows_its_text_is_caught() {
        let mut t = MUR;
        t.diff_add_bg = Style::new().bg(INK);
        let f = check(&t, ASSUMED_BG_MUR);
        assert!(pairs(&f).contains(&("diff_add_text", "diff_add_bg")), "{f:?}");
    }

    #[test]
    fn a_stripe_the_colour_of_the_text_is_caught() {
        let mut t = MUR;
        t.surface_alt = Style::new().bg(INK);
        let f = check(&t, ASSUMED_BG_MUR);
        assert!(pairs(&f).contains(&("text", "surface_alt")), "{f:?}");
    }

    /// DIM is measured as what the terminal draws, not as the colour named.
    #[test]
    fn dim_is_measured_dimmed() {
        let mut t = LIGHT;
        t.muted = t.muted.add_modifier(Modifier::DIM);
        let f = check(&t, ASSUMED_BG_LIGHT);
        assert!(pairs(&f).contains(&("muted", "assumed_bg")), "{f:?}");
    }

    #[test]
    fn an_unreadable_badge_warn_is_caught() {
        let mut t = MUR;
        let amber = Color::Rgb(0xf2, 0xc7, 0x6a);
        t.badge_warn = Style::new().fg(amber).bg(amber);
        let f = check(&t, ASSUMED_BG_MUR);
        assert!(pairs(&f).contains(&("badge_warn", "its own bg")), "{f:?}");
    }

    /// A named slot is the terminal's to choose; murmur cannot measure it.
    #[test]
    fn named_slots_are_not_measured() {
        let mut t = MUR;
        t.warn = Style::new().fg(Color::Yellow);
        assert!(check(&t, ASSUMED_BG_MUR).iter().all(|f| f.fg != "warn"));
    }

    #[test]
    fn a_failure_reads_as_one_clause() {
        let f = Failure {
            fg: "warn",
            on: "assumed_bg",
            ratio: 2.14,
            min: MIN_TEXT,
        };
        assert_eq!(f.to_string(), "warn 2.1:1 on assumed_bg (needs 4.5:1)");
    }
}
```

In `CLI/theme/mod.rs` replace

```rust
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;
```

with

```rust
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

pub mod contrast;
```

- [ ] Watch it fail:

```bash
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::theme::contrast::/)'
```

Expected: 4 failed (`a_diff_tint…`, `a_stripe…`, `dim_is_measured_dimmed`,
`an_unreadable_badge_warn…`), 2 passed.

- [ ] Implement `check`. Replace the stub

```rust
pub fn check(_theme: &Theme, _assumed_bg: Color) -> Vec<Failure> {
    Vec::new()
}
```

with

```rust
/// What DIM does on most terminals: the ink pulled halfway to the ground.
/// Measuring the undimmed colour is how a faint token used to pass.
fn dimmed(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    std::array::from_fn(|i| ((u16::from(fg[i]) + u16::from(bg[i])) / 2) as u8)
}

/// Every pair `theme` paints, measured on `assumed_bg`. Empty means pass.
///
/// A token's ground is its own `bg`, else the surface named in the pair,
/// else `assumed_bg` — so an unset surface collapses to the terminal and
/// every pair is defined. A pair with a named or default colour on either
/// side is skipped.
pub fn check(theme: &Theme, assumed_bg: Color) -> Vec<Failure> {
    let bare = Style::new();
    let mut pairs: Vec<(&'static str, Style, &'static str, Style, f64)> = Vec::new();
    for (on, ground) in [
        ("assumed_bg", bare),
        ("surface", theme.surface),
        ("surface_alt", theme.surface_alt),
    ] {
        pairs.push(("text", theme.text, on, ground, MIN_BODY));
        for (fg, style) in [
            ("muted", theme.muted),
            ("emphasis", theme.emphasis),
            ("accent", theme.accent),
            ("accent_alt", theme.accent_alt),
            ("ok", theme.ok),
            ("warn", theme.warn),
            ("error", theme.error),
        ] {
            pairs.push((fg, style, on, ground, MIN_TEXT));
        }
    }
    let settle = theme.settlement_surface;
    pairs.extend([
        ("diff_add_text", theme.diff_add_text, "diff_add_bg", theme.diff_add_bg, MIN_TEXT),
        ("diff_del_text", theme.diff_del_text, "diff_del_bg", theme.diff_del_bg, MIN_TEXT),
        ("diff_add_mark", theme.diff_add_mark, "diff_add_bg", theme.diff_add_bg, MIN_GLYPH),
        ("diff_del_mark", theme.diff_del_mark, "diff_del_bg", theme.diff_del_bg, MIN_GLYPH),
        ("settlement_text", theme.settlement_text, "settlement_surface", settle, MIN_BODY),
        ("settlement_muted", theme.settlement_muted, "settlement_surface", settle, MIN_TEXT),
        ("settlement_accent", theme.settlement_accent, "settlement_surface", settle, MIN_TEXT),
        ("settlement_ok", theme.settlement_ok, "settlement_surface", settle, MIN_TEXT),
        ("settlement_warn", theme.settlement_warn, "settlement_surface", settle, MIN_TEXT),
        ("settlement_error", theme.settlement_error, "settlement_surface", settle, MIN_TEXT),
        ("badge", theme.badge, "its own bg", bare, MIN_TEXT),
        ("badge_warn", theme.badge_warn, "its own bg", bare, MIN_TEXT),
    ]);
    pairs
        .into_iter()
        .filter_map(|(fg, style, on, ground, min)| {
            let bg = rgb(style.bg.or(ground.bg).or(Some(assumed_bg)))?;
            let mut ink = rgb(style.fg)?;
            if style.add_modifier.contains(Modifier::DIM) {
                ink = dimmed(ink, bg);
            }
            let ratio = ratio_rgb(ink, bg);
            (ratio < min).then_some(Failure { fg, on, ratio, min })
        })
        .collect()
}
```

- [ ] Hold the built-ins to it. In `CLI/theme/tests.rs` replace the two
local helpers

```rust
/// WCAG 2 relative luminance of an sRGB colour.
fn luminance(c: Color) -> f64 {
```

through the end of `fn contrast_ratio(a: Color, b: Color) -> f64 { … }`
(the two functions, 21 lines) with

```rust
fn contrast_ratio(a: Color, b: Color) -> f64 {
    super::contrast::ratio(a, b).expect("contrast is only defined for Rgb tokens")
}
```

and replace the whole `rgb_skins_meet_wcag` test (its doc comment through
its closing brace) with

```rust
/// The truecolor skins assume a background, and every foreground must read
/// against the ground it is actually painted on — the terminal, the status
/// surface, the table stripe, a diff tint, the settlement card, a chip's own
/// fill. One function decides, the same one user skin files go through.
#[test]
fn rgb_skins_meet_wcag() {
    for (name, theme, bg) in [
        ("light", &LIGHT, ASSUMED_BG_LIGHT),
        ("mur", &MUR, ASSUMED_BG_MUR),
        ("clay", &CLAY, ASSUMED_BG_CLAY),
    ] {
        let failures = super::contrast::check(theme, bg);
        assert!(
            failures.is_empty(),
            "{name}: {}",
            failures
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
}
```

- [ ] Watch it pass:

```bash
cargo fmt --all
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::theme::/)'
```

Expected: all pass. If `rgb_skins_meet_wcag` fails, the message names the
pair and ratio; stop and report it — do not change a palette value without
recording the new ratio in the spec §1 table.

- [ ] Commit:

```bash
git add mur-core/src/cmd/agent/cli/theme
git commit -m "feat(murmur): contrast check over painted pairs; built-in skins held to it"
```

---

## Task 4: Markdown paints with the skin

**Interfaces** — Consumes: Task 2 (`surface_alt`). Produces:

```rust
// CLI/markdown.rs
pub fn render(src: &str, width: usize, theme: &'static Theme) -> Text<'static>;
// CLI/app/msg.rs
pub(super) fn agent_rendered(text: String, width: usize, theme: &'static crate::cmd::agent::cli::theme::Theme) -> ChatMsg;
```

Colour mapping: quote bar `muted`; `---` rule and table grid `border`; list
marker `accent`; table header `accent` + BOLD; stripe `surface_alt`
(patched, so an empty `ansi` value draws nothing). `CODE` stays.

- [ ] Write the failing tests. Create `CLI/markdown/theme_tests.rs`:

```rust
//! The markdown renderer paints only with the active skin (murmur UI spec
//! §1): stripe `surface_alt`, grid and rule `border`, quote `muted`, list
//! marker and table header `accent`.

use super::{RULE, render};
use crate::cmd::agent::cli::theme::{ANSI, CLAY, LIGHT, MUR};
use ratatui::style::{Color, Style};
use ratatui::text::{Span, Text};

const DOC: &str = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n| 5 | 6 |\n\n> quoted\n\n---\n\n- item\n";
const WIDTH: usize = 60;

/// Each span's effective style: the line's style under the span's own.
fn styles(t: &Text<'static>) -> Vec<Style> {
    t.lines
        .iter()
        .flat_map(|l| l.spans.iter().map(move |s| l.style.patch(s.style)))
        .collect()
}

fn span<'a>(t: &'a Text<'static>, pred: impl Fn(&Span) -> bool) -> (&'a Span<'static>, Style) {
    t.lines
        .iter()
        .flat_map(|l| l.spans.iter().map(move |s| (s, l.style.patch(s.style))))
        .find(|(s, _)| pred(s))
        .expect("span rendered")
}

#[test]
fn ansi_markdown_pins_no_colour_and_draws_no_stripe() {
    let t = render(DOC, WIDTH, &ANSI);
    for s in styles(&t) {
        assert!(s.bg.is_none(), "ansi painted a background: {s:?}");
        if let Some(c) = s.fg {
            assert!(
                !matches!(c, Color::Rgb(..) | Color::Indexed(_)),
                "ansi markdown pinned {c:?}"
            );
        }
    }
}

#[test]
fn rgb_skins_stripe_with_surface_alt_only() {
    for theme in [&LIGHT, &MUR, &CLAY] {
        let want = theme.surface_alt.bg;
        let t = render(DOC, WIDTH, theme);
        let bgs: Vec<_> = styles(&t).into_iter().filter_map(|s| s.bg).collect();
        assert!(!bgs.is_empty(), "no stripe painted, want {want:?}");
        assert!(
            bgs.iter().all(|b| Some(*b) == want),
            "a background other than surface_alt: {bgs:?}"
        );
    }
}

#[test]
fn grid_rule_quote_marker_and_header_take_their_tokens() {
    let t = render(DOC, WIDTH, &MUR);
    let (_, grid) = span(&t, |s| s.content.starts_with('╭'));
    assert_eq!(grid.fg, MUR.border.fg, "table grid");
    let (_, rule) = span(&t, |s| s.content == RULE);
    assert_eq!(rule.fg, MUR.border.fg, "horizontal rule");
    let (_, quote) = span(&t, |s| s.content == "▏ ");
    assert_eq!(quote.fg, MUR.muted.fg, "quote bar");
    let (_, marker) = span(&t, |s| s.content == "• ");
    assert_eq!(marker.fg, MUR.accent.fg, "list marker");
    let (_, header) = span(&t, |s| s.content == "a");
    assert_eq!(header.fg, MUR.accent.fg, "table header colour");
    assert!(
        header.add_modifier.contains(ratatui::style::Modifier::BOLD),
        "table header weight"
    );
}
```

At the end of `CLI/markdown.rs` (after the existing test module's closing
brace) add

```rust

#[cfg(test)]
mod theme_tests;
```

and in the existing test module replace

```rust
    fn render(src: &str) -> Text<'static> {
        super::render(src, 80)
    }
```

with

```rust
    fn render(src: &str) -> Text<'static> {
        super::render(src, 80, &crate::cmd::agent::cli::theme::ANSI)
    }
```

- [ ] Watch it fail:

```bash
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::markdown::/)'
```

Expected: compile error — `render` takes 2 arguments but 3 were supplied.

- [ ] Thread the theme through the renderer. In `CLI/markdown.rs`:

Replace

```rust
use unicode_width::UnicodeWidthChar;
```

with

```rust
use unicode_width::UnicodeWidthChar;

use super::theme::{ANSI, Theme};
```

Replace

```rust
const HEADING: Color = Color::Cyan;
const CODE: Color = Color::Yellow;
const QUOTE: Color = Color::DarkGray;
/// Zebra-stripe background for alternating table body rows. Kept at the
/// terminal's own dim index so it reads on both light and dark schemes.
const STRIPE: Color = Color::Indexed(236);
```

with

```rust
// ponytail: inline code keeps a pinned named slot until the code-block
// design picks its token (murmur UI spec §1); every other colour in this
// file comes from the skin.
const CODE: Color = Color::Yellow;
```

Replace

```rust
pub fn render(src: &str, width: usize) -> Text<'static> {
    let mut r = Renderer {
        width: width.max(MIN_BODY_COLS),
        ..Renderer::default()
    };
```

with

```rust
pub fn render(src: &str, width: usize, theme: &'static Theme) -> Text<'static> {
    let mut r = Renderer {
        width: width.max(MIN_BODY_COLS),
        skin: Skin(theme),
        ..Renderer::default()
    };
```

Replace

```rust
#[derive(Default)]
struct Renderer {
    /// Columns available to a table (see `render`).
    width: usize,
```

with

```rust
/// The skin a render paints with. A newtype only so `Renderer` keeps
/// `#[derive(Default)]`; `render` always sets it.
#[derive(Clone, Copy)]
struct Skin(&'static Theme);

impl Default for Skin {
    fn default() -> Self {
        Skin(&ANSI)
    }
}

#[derive(Default)]
struct Renderer {
    /// Columns available to a table (see `render`).
    width: usize,
    skin: Skin,
```

Replace

```rust
                .push(Span::styled("▏ ".to_string(), Style::default().fg(QUOTE)));
```

with

```rust
                .push(Span::styled("▏ ".to_string(), self.skin.0.muted));
```

Replace

```rust
                    .push(Line::styled(RULE.to_string(), Style::default().fg(QUOTE)));
```

with

```rust
                    .push(Line::styled(RULE.to_string(), self.skin.0.border));
```

Replace

```rust
                    .push(Span::styled(marker, Style::default().fg(HEADING)));
```

with

```rust
                    .push(Span::styled(marker, self.skin.0.accent));
```

Replace

```rust
        let rule = |l: &str, m: &str, r: &str| -> Line<'static> {
            let bars = widths
                .iter()
                .map(|w| "─".repeat(w + 2 * CELL_PAD))
                .collect::<Vec<_>>()
                .join(m);
            Line::styled(format!("{l}{bars}{r}"), Style::default().fg(QUOTE))
        };
        let border = Style::default().fg(QUOTE);
```

with

```rust
        let border = self.skin.0.border;
        let header = self.skin.0.accent.add_modifier(Modifier::BOLD);
        let stripe_bg = self.skin.0.surface_alt;
        let rule = |l: &str, m: &str, r: &str| -> Line<'static> {
            let bars = widths
                .iter()
                .map(|w| "─".repeat(w + 2 * CELL_PAD))
                .collect::<Vec<_>>()
                .join(m);
            Line::styled(format!("{l}{bars}{r}"), border)
        };
```

Replace

```rust
                            Span::styled(
                                s.content,
                                s.style.fg(HEADING).add_modifier(Modifier::BOLD),
                            )
```

with

```rust
                            Span::styled(s.content, s.style.patch(header))
```

Replace

```rust
                            Span::styled(s.content, style.bg(STRIPE))
```

with

```rust
                            Span::styled(s.content, style.patch(stripe_bg))
```

- [ ] Pass the theme at every caller.

`CLI/ui/message.rs` — replace

```rust
        markdown::render(text, markdown::body_cols(width, theme.inner_padding))
```

with

```rust
        markdown::render(text, markdown::body_cols(width, theme.inner_padding), theme)
```

`CLI/app/msg.rs` — replace

```rust
    pub(super) fn agent_rendered(text: String, width: usize) -> Self {
        let (text, settlement) = super::super::settlement::split(&text);
        let rendered = Some(markdown::render(&text, width).lines);
```

with

```rust
    pub(super) fn agent_rendered(
        text: String,
        width: usize,
        theme: &'static crate::cmd::agent::cli::theme::Theme,
    ) -> Self {
        let (text, settlement) = super::super::settlement::split(&text);
        let rendered = Some(markdown::render(&text, width, theme).lines);
```

`CLI/app/mod.rs` — replace

```rust
                self.messages.push(ChatMsg::agent_rendered(t.text, width));
```

with

```rust
                self.messages
                    .push(ChatMsg::agent_rendered(t.text, width, self.theme));
```

`CLI/app/transcript.rs` — replace

```rust
        let width = self.body_cols();
        let mut body = None;
```

with

```rust
        let width = self.body_cols();
        let theme = self.theme;
        let mut body = None;
```

replace

```rust
            m.rendered = Some(markdown::render(&m.text, width).lines);
            body = Some(m.text.clone());
```

with

```rust
            m.rendered = Some(markdown::render(&m.text, width, theme).lines);
            body = Some(m.text.clone());
```

replace

```rust
                .push(ChatMsg::agent_rendered(reply.clone(), width));
```

with

```rust
                .push(ChatMsg::agent_rendered(reply.clone(), width, theme));
```

and replace

```rust
                    Some(markdown::render(&self.messages[i].text, self.body_cols()).lines);
```

with

```rust
                    Some(markdown::render(&self.messages[i].text, self.body_cols(), self.theme).lines);
```

`CLI/app/usage.rs` — replace

```rust
        let width = self.body_cols();
        for m in &mut self.messages {
            if m.rendered.is_some() {
                m.rendered = Some(markdown::render(&m.text, width).lines);
```

with

```rust
        let width = self.body_cols();
        let theme = self.theme;
        for m in &mut self.messages {
            if m.rendered.is_some() {
                m.rendered = Some(markdown::render(&m.text, width, theme).lines);
```

- [ ] Confirm no caller was missed:

```bash
command grep -rn "markdown::render(\|agent_rendered(" --include='*.rs' mur-core/src | command grep -v "fn render\|fn agent_rendered"
```

Expected: every hit shows three arguments.

- [ ] Watch it pass:

```bash
cargo fmt --all
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::/)'
```

Expected: all pass, including the three `markdown::theme_tests`.

- [ ] Commit:

```bash
git add mur-core/src/cmd/agent/cli
git commit -m "feat(murmur): markdown paints with the skin; ansi draws no stripe"
```

---

## Task 5: `App::apply_theme` — one door for a skin switch

**Interfaces** — Consumes: Task 4 (`render(…, theme)`). Produces:

```rust
// CLI/app/usage.rs, impl App
pub fn apply_theme(&mut self, theme: &'static crate::cmd::agent::cli::theme::Theme);
// CLI/app/mod.rs
fn new_input(theme: &'static Theme) -> TextArea<'static>;
```

PR-4's runtime `/skin` load and the spec §2 `/skin` notice call
`apply_theme`; nothing else assigns `app.theme` outside tests.

- [ ] Write the failing tests. Create `CLI/app/tests/theme_tests.rs`:

```rust
//! `App::apply_theme`: a skin switch repaints everything murmur still owns
//! — cached markdown (its stripe is baked in) and the composer placeholder.

use super::super::*;
use crate::cmd::agent::cli::theme::{ANSI, MUR};
use ratatui::style::Color;

const TABLE: &str = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";

fn backgrounds(app: &App) -> Vec<Color> {
    app.messages
        .iter()
        .filter_map(|m| m.rendered.as_ref())
        .flatten()
        .flat_map(|l| l.spans.iter().filter_map(|s| s.style.bg))
        .collect()
}

#[test]
fn apply_theme_restripes_cached_tables() {
    let mut app = App::test_fixture();
    let width = app.body_cols();
    app.messages
        .push(ChatMsg::agent_rendered(TABLE.into(), width, &ANSI));
    assert!(backgrounds(&app).is_empty(), "ansi must not stripe");

    app.apply_theme(&MUR);

    assert!(std::ptr::eq(app.theme, &MUR));
    let bgs = backgrounds(&app);
    assert!(!bgs.is_empty(), "the cached table was not re-rendered");
    assert!(bgs.iter().all(|b| Some(*b) == MUR.surface_alt.bg), "{bgs:?}");
}

#[test]
fn the_placeholder_follows_the_skin_and_survives_a_clear() {
    let mut app = App::test_fixture();
    assert_eq!(app.input.placeholder_style(), Some(ANSI.muted));
    app.apply_theme(&MUR);
    assert_eq!(app.input.placeholder_style(), Some(MUR.muted));
    app.clear_input();
    assert_eq!(app.input.placeholder_style(), Some(MUR.muted));
    app.set_input("x");
    assert_eq!(app.input.placeholder_style(), Some(MUR.muted));
}
```

In `CLI/app/tests/mod.rs` replace

```rust
mod step_app_tests;
```

with

```rust
mod step_app_tests;
mod theme_tests;
```

- [ ] Watch it fail:

```bash
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::app::tests::theme_tests::/)'
```

Expected: compile error — no method named `apply_theme`.

- [ ] Implement. In `CLI/app/mod.rs` replace

```rust
fn new_input() -> TextArea<'static> {
```

with

```rust
fn new_input(theme: &'static Theme) -> TextArea<'static> {
```

replace

```rust
    ta.set_placeholder_style(Style::default().fg(Color::DarkGray));
```

with

```rust
    ta.set_placeholder_style(theme.muted);
```

replace

```rust
            input: new_input(),
```

with

```rust
            input: new_input(theme),
```

and replace

```rust
use ratatui::style::{Color, Style};
```

with

```rust
use ratatui::style::Style;
```

In `CLI/app/usage.rs` replace both occurrences of

```rust
        self.input = new_input();
```

with

```rust
        self.input = new_input(self.theme);
```

and after the closing brace of `pub fn rerender_markdown(&mut self)` add

```rust

    /// Switch skin for everything murmur can still repaint: the theme, every
    /// cached markdown render (a table's stripe is baked in when it
    /// finishes), and the composer placeholder. Rows already written to the
    /// terminal's scrollback keep their colours; nothing can reach them.
    pub fn apply_theme(&mut self, theme: &'static crate::cmd::agent::cli::theme::Theme) {
        self.theme = theme;
        self.rerender_markdown();
        self.input.set_placeholder_style(theme.muted);
    }
```

In `CLI/slash_cmds.rs` replace

```rust
                    app.theme = theme::resolve_skin(&name);
```

with

```rust
                    app.apply_theme(theme::resolve_skin(&name));
```

- [ ] Watch it pass:

```bash
cargo fmt --all
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::/)'
wc -l mur-core/src/cmd/agent/cli/slash_cmds.rs mur-core/src/cmd/agent/cli/app/mod.rs
```

Expected: all pass; both files still ≤ 800 lines.

- [ ] Commit:

```bash
git add mur-core/src/cmd/agent/cli
git commit -m "feat(murmur): App::apply_theme re-renders cached markdown and the placeholder on /skin"
```

---

## Task 6: Status row — chips and states from the skin

**Interfaces** — Consumes: Task 2 (`badge_warn`). Produces: test module
`CLI/ui/theme_paint_tests.rs` with helpers `status_buffer(theme, auto_all)`
and `assert_painted(buf, needle, want)`, extended in Task 8.

- [ ] Write the failing tests. Create `CLI/ui/theme_paint_tests.rs`:

```rust
//! Paint-site guards for the murmur UI spec §1: every colour a frame shows
//! comes from the active skin.

use super::status::render_status;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::theme::{ANSI, CLAY, LIGHT, MUR, Theme};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};

const STATUS_W: u16 = 140;

/// The status row with every chip up. `auto_all` false: READS (ask mode,
/// read lane on) and AUTO:bash (one session-allowed tool). `auto_all` true:
/// the global AUTO chip. Both carry MONITOR (2) with one issue.
fn status_buffer(theme: &'static Theme, auto_all: bool) -> Buffer {
    let mut app = App::test_fixture();
    app.theme = theme;
    app.auto_approve = auto_all;
    app.auto_reads = true;
    app.session_tool_allow.insert("bash".into());
    app.monitor_total = 2;
    app.monitor_conditions = 1;
    let mut term = Terminal::new(TestBackend::new(STATUS_W, 1)).unwrap();
    term.draw(|f| render_status(f, &app, f.area())).unwrap();
    term.backend().buffer().clone()
}

/// The cell under the first character of `needle` on row 0 is painted
/// `want`: same foreground and background (unset means the terminal's), and
/// at least `want`'s modifiers.
fn assert_painted(buf: &Buffer, needle: &str, want: Style) {
    let row: String = (0..buf.area.width).map(|x| buf[(x, 0)].symbol()).collect();
    let at = row
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not on the row: {row:?}"));
    let x = row[..at].chars().count() as u16;
    let got = buf[(x, 0)].style();
    assert_eq!(
        (got.fg, got.bg),
        (
            Some(want.fg.unwrap_or(Color::Reset)),
            Some(want.bg.unwrap_or(Color::Reset))
        ),
        "{needle:?}: colours"
    );
    assert!(
        got.add_modifier.contains(want.add_modifier),
        "{needle:?}: modifiers {:?}, want {:?}",
        got.add_modifier,
        want.add_modifier
    );
}

#[test]
fn chips_and_states_take_their_tokens_on_every_skin() {
    for theme in [&ANSI, &LIGHT, &MUR, &CLAY] {
        let buf = status_buffer(theme, false);
        assert_painted(&buf, "READS", theme.badge);
        assert_painted(&buf, "AUTO:bash", theme.badge_warn);
        assert_painted(&buf, "MONITOR", theme.badge);
        assert_painted(&buf, "1 issue", theme.error);
        let buf = status_buffer(theme, true);
        assert_painted(&buf, "AUTO ", theme.badge_warn);
    }
}

/// Spec §7 `ansi_render_has_no_rgb`, status-row part: no pinned colour
/// anywhere on it. (The approval panel joins this guard in PR-3.)
#[test]
fn the_ansi_status_row_pins_no_colour() {
    for auto_all in [false, true] {
        let buf = status_buffer(&ANSI, auto_all);
        for cell in buf.content() {
            for c in [cell.fg, cell.bg] {
                assert!(
                    !matches!(c, Color::Rgb(..) | Color::Indexed(_)),
                    "ansi status painted {c:?} under {:?}",
                    cell.symbol()
                );
            }
        }
    }
}
```

In `CLI/ui.rs` replace

```rust
mod status;
```

with

```rust
mod status;
#[cfg(test)]
mod theme_paint_tests;
```

- [ ] Watch it fail:

```bash
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::ui::theme_paint_tests::/)'
```

Expected: 2 failed — `chips_and_states…` on `"READS": colours` (pinned
Black on Cyan), `the_ansi_status_row…` on `Rgb(255, 165, 0)`.

- [ ] Implement. In `CLI/ui/status.rs`:

Replace

```rust
            Style::default().fg(Color::Yellow),
```

with

```rust
            theme.warn,
```

Replace

```rust
        spans.push(Span::styled(
            " AUTO ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
```

with

```rust
        spans.push(Span::styled(" AUTO ", theme.badge_warn));
```

Replace

```rust
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
```

with

```rust
        spans.push(Span::styled(label, theme.badge_warn));
```

Replace

```rust
        spans.push(Span::styled(
            " READS ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
```

with

```rust
        spans.push(Span::styled(" READS ", theme.badge));
```

Replace

```rust
        spans.push(Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(255, 165, 0))
                .add_modifier(Modifier::BOLD),
        ));
```

with

```rust
        spans.push(Span::styled(format!(" {label} "), theme.badge));
```

Replace

```rust
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
```

with

```rust
                theme.error.add_modifier(Modifier::BOLD),
```

Replace

```rust
use ratatui::style::{Color, Modifier, Style};
```

with

```rust
use ratatui::style::{Modifier, Style};
```

- [ ] Watch it pass:

```bash
cargo fmt --all
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::ui::/)'
```

Expected: all pass.

- [ ] Commit:

```bash
git add mur-core/src/cmd/agent/cli/ui.rs mur-core/src/cmd/agent/cli/ui
git commit -m "feat(murmur): status chips take badge/badge_warn; countdown and issue take warn/error"
```

---

## Task 7: `DarkGray` → `muted`

**Interfaces** — Consumes: nothing new. Produces: no `Color::DarkGray`
left in `ui.rs`, `ui/chooser.rs`, `ui/hitl.rs`.

This task is a mechanical token swap with no new behaviour to test; the
check is the grep and the existing suites.

- [ ] `CLI/ui/chooser.rs` — replace

```rust
                        format!(" — {}", c.desc),
                        Style::default().fg(Color::DarkGray),
```

with

```rust
                        format!(" — {}", c.desc),
                        theme.muted,
```

replace

```rust
                        format!("   {}", c.desc),
                        Style::default().fg(Color::DarkGray),
```

with

```rust
                        format!("   {}", c.desc),
                        theme.muted,
```

and replace

```rust
use ratatui::style::{Color, Modifier, Style};
```

with

```rust
use ratatui::style::Modifier;
```

- [ ] `CLI/ui.rs` — replace

```rust
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
```

with

```rust
            app.theme.muted.add_modifier(Modifier::ITALIC),
```

replace

```rust
                        format!("   {}", c.desc), // align under the label (past "N  ")
                        Style::default().fg(Color::DarkGray),
```

with

```rust
                        format!("   {}", c.desc), // align under the label (past "N  ")
                        theme.muted,
```

replace

```rust
                        c.desc.clone(),
                        Style::default().fg(Color::DarkGray),
```

with

```rust
                        c.desc.clone(),
                        theme.muted,
```

and replace

```rust
use ratatui::style::{Color, Modifier, Style};
```

with

```rust
use ratatui::style::{Modifier, Style};
```

- [ ] `CLI/ui/hitl.rs` (the `DarkGray` sites only; the rest is PR-3's) —
replace

```rust
                Span::styled("tool: ", Style::default().fg(Color::DarkGray)),
```

with

```rust
                Span::styled("tool: ", theme.muted),
```

replace

```rust
                Span::styled(intent.to_string(), Style::default().fg(Color::DarkGray)),
```

with

```rust
                Span::styled(intent.to_string(), theme.muted),
```

replace

```rust
            body.push(Line::styled(row, Style::default().fg(Color::DarkGray)));
```

with

```rust
            body.push(Line::styled(row, theme.muted));
```

replace

```rust
        "   ↑/↓ select · Enter confirm · 1-4 pick directly · Esc deny",
        Style::default().fg(Color::DarkGray),
```

with

```rust
        "   ↑/↓ select · Enter confirm · 1-4 pick directly · Esc deny",
        theme.muted,
```

and replace

```rust
        lines.push(Line::styled(note, Style::default().fg(Color::DarkGray)));
```

with

```rust
        lines.push(Line::styled(note, theme.muted));
```

- [ ] Verify:

```bash
command grep -rn "DarkGray" mur-core/src/cmd/agent/cli --include='*.rs' | command grep -v "diff.rs"
cargo fmt --all
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::/)'
```

Expected: the grep prints nothing (`diff.rs`'s `DarkGray` is an ANSI
escape table entry, not a paint site); all tests pass.

- [ ] Commit:

```bash
git add mur-core/src/cmd/agent/cli
git commit -m "fix(murmur): secondary text takes muted, not a pinned DarkGray"
```

---

## Task 8: User body text `muted`; tool error `error`

**Interfaces** — Consumes: Task 6's `theme_paint_tests.rs`. Produces:
`render_card::error_line(card, theme)` (private; signature change only).

- [ ] Write the failing tests. Append to `CLI/ui/theme_paint_tests.rs`:

```rust
use super::message::push_message;
use crate::cmd::agent::cli::app::{ChatMsg, Role};
use crate::cmd::agent::cli::render_card::card_lines;
use crate::cmd::agent::cli::step::{CallOutcome, StepCard};
use ratatui::style::Modifier;
use ratatui::text::Line;

/// Effective style of the first span whose text contains `needle`.
fn style_in(lines: &[Line<'static>], needle: &str) -> Style {
    lines
        .iter()
        .flat_map(|l| l.spans.iter().map(move |s| (s, l.style.patch(s.style))))
        .find(|(s, _)| s.content.contains(needle))
        .map(|(_, st)| st)
        .unwrap_or_else(|| panic!("{needle:?} not rendered"))
}

/// Decision 4: a user turn reads in `muted` — a measured colour on RGB
/// skins, DIM on `ansi` — never `text` + DIM.
#[test]
fn user_body_is_muted() {
    for theme in [&ANSI, &LIGHT, &MUR, &CLAY] {
        let m = ChatMsg::for_test(Role::User, "hello there");
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, theme, false, 80);
        let got = style_in(&lines, "hello there");
        assert_eq!(got.fg, theme.muted.fg, "user body colour");
        assert_eq!(
            got.add_modifier.contains(Modifier::DIM),
            theme.muted.add_modifier.contains(Modifier::DIM),
            "user body DIM follows muted"
        );
    }
}

/// A failed call's header and error line take the skin's `error`, not a
/// pinned red.
#[test]
fn a_failed_card_takes_error() {
    for theme in [&ANSI, &LIGHT, &MUR, &CLAY] {
        let mut c = StepCard::new("s1".into(), "edit_file".into(), serde_json::json!({}));
        c.complete(CallOutcome::Failed, String::new(), false, 0, Some("boom".into()), 19);
        let lines = card_lines(&c, theme, false, 80);
        assert_eq!(style_in(&lines, "edit_file").fg, theme.error.fg, "header");
        assert_eq!(style_in(&lines, "boom").fg, theme.error.fg, "error line");
    }
}
```

- [ ] Watch it fail:

```bash
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::ui::theme_paint_tests::/)'
```

Expected: `user_body_is_muted` fails on `light` (`Rgb(31, 36, 48)` vs
`Rgb(92, 99, 112)`); `a_failed_card_takes_error` fails on `light`
(`Red` vs `Rgb(179, 38, 30)`).

- [ ] Implement. `CLI/ui/message.rs` — replace

```rust
            for l in m.text.lines() {
                lines.push(Line::styled(
                    format!("{MSG_INDENT}{l}"),
                    theme.text.add_modifier(Modifier::DIM),
                ));
            }
```

with

```rust
            for l in m.text.lines() {
                lines.push(Line::styled(format!("{MSG_INDENT}{l}"), theme.muted));
            }
```

`CLI/render_card.rs` — replace

```rust
        StepState::Error => Style::default().fg(ratatui::style::Color::Red),
```

with

```rust
        StepState::Error => theme.error,
```

replace

```rust
fn error_line(card: &StepCard) -> Option<Line<'static>> {
    card.error.as_ref().map(|err| {
        Line::styled(
            format!(" ✗ {err}"),
            Style::default().fg(ratatui::style::Color::Red),
        )
    })
}
```

with

```rust
fn error_line(card: &StepCard, theme: &'static Theme) -> Option<Line<'static>> {
    card.error
        .as_ref()
        .map(|err| Line::styled(format!(" ✗ {err}"), theme.error))
}
```

and replace both occurrences of

```rust
    if let Some(line) = error_line(card) {
```

with

```rust
    if let Some(line) = error_line(card, theme) {
```

- [ ] Watch it pass:

```bash
cargo fmt --all
cargo nextest run -p mur-core -E 'test(/cmd::agent::cli::/)'
```

Expected: all pass. If a pre-existing test in `ui/message.rs` or
`render_card.rs` asserted the old DIM or red, it fails here: update that
assertion to the token (`theme.muted` / `theme.error`) and say so in the
commit body.

- [ ] Commit:

```bash
git add mur-core/src/cmd/agent/cli
git commit -m "fix(murmur): user turns read in muted; failed tool calls take the skin's error"
```

---

## Task 9: Gate, look, and PR

**Interfaces** — Consumes: Tasks 1–8. Produces: a PR against `main`.

- [ ] CI's lint and the full suite:

```bash
cargo fmt --all -- --check
cargo clippy --all --all-targets --no-deps --locked -- -D warnings; echo "clippy exit=$?"
cargo nextest run -p mur-core; echo "nextest exit=$?"
```

Expected: fmt prints nothing; `clippy exit=0`; `nextest exit=0`. Read the
exit codes, not a filtered grep.

- [ ] File-size rule:

```bash
wc -l mur-core/src/cmd/agent/cli/{theme/mod.rs,theme/tests.rs,theme/contrast.rs,markdown.rs,slash_cmds.rs,app/mod.rs,ui/status.rs,render_card.rs}
```

Expected: every count ≤ 800.

- [ ] Look at it, all four skins, the way the spec was reviewed: a real
session driven in tmux, displayed in kitty (Ghostty hosts Claude Code and
is masked from computer-use screenshots). Note the saved skin first so you
can prove `--skin` did not change it:

```bash
grep 'skin:' ~/.mur/config.yaml
cargo build -p mur-core --bin mur
BIN="$PWD/target/debug/mur"
shot() {  # shot <skin> <terminal bg> <terminal fg>
  tmux kill-session -t murui 2>/dev/null
  tmux new-session -d -s murui -x 150 -y 45 "$BIN agent cli mur --skin $1 --ask --resume"
  tmux set -t murui status off
  open -na kitty --args -o font_size=13 -o background="$2" -o foreground="$3" tmux attach -t murui
  sleep 4
}
```

Run `shot` once per row below, take a computer-use screenshot of the kitty
window (request access to `kitty` once), then close it with
`pkill -f "kitty.*tmux attach -t murui"` before the next:

| skin | bg | fg |
|---|---|---|
| ansi | `#fafafa` | `#222222` |
| ansi | `#1e1e1e` | `#dddddd` |
| light | `#ffffff` | `#1f2430` |
| mur | `#0b0b1a` | `#e4e4f4` |
| clay | `#1a1a1a` | `#ffffff` |

The resumed conversation must contain a markdown table. If it does not,
in the first session send one prompt that produces one
(`tmux send-keys -t murui -l '用 markdown 表格列出三種顏色和 hex' && tmux send-keys -t murui Enter`),
wait for the reply, and re-run that row; later rows resume it.

On each screenshot check: the table stripes in the skin's `surface_alt`
(none on `ansi`, and on the light terminal no row turns dark); the status
chips are in the skin's own colours (no cyan slab on `mur` or `clay`); user
turns are readable on `light`. Afterwards `tmux kill-session -t murui` and
re-run `grep 'skin:' ~/.mur/config.yaml` — it must print the same line as
before.

- [ ] Push and open the PR:

```bash
git push -u origin feat/murmur-theme-tokens
gh pr create --base main --title "feat(murmur): every colour through the theme (UI pass PR-1)" --body "$(cat <<'EOF'
PR-1 of the murmur UI pass (spec: docs/superpowers/specs/2026-09-29-murmur-ui-p1p2-design.md §1).

- New tokens `surface_alt` (table stripe) and `badge_warn` (AUTO chip); `ansi` paints no decorative background.
- One contrast check (`theme::contrast::check`) over every pair a skin paints — terminal, status surface, stripe, diff tint, settlement card, chip fill; DIM measured as dimmed. Built-in skins pass it; user skins (PR-4) will load through it.
- Markdown renders with the theme; `/skin` goes through `App::apply_theme`, which re-renders cached tables and restyles the placeholder.
- Status chips, countdown, monitor issue, secondary text, user turns and tool errors take tokens instead of pinned colours.

Visible change: status chips in the skin's colours; table stripe from the skin (none on `ansi`, which fixes striped rows vanishing on light terminals); user turns readable on `light`; table headers and list markers in `accent`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] Before any merge, check CI independently (`gh pr checks <n>`; zero
pending, zero failing). Do not use `--auto`.

---

## Self-review (done while writing)

- **Spec coverage (PR-1 scope):** §1 new tokens → Task 2; §1 paint-site
  table: `STRIPE`/`QUOTE`/`HEADING` → Task 4, status chips/countdown/issue →
  Task 6, `DarkGray` sites + placeholder → Tasks 5/7, user body text →
  Task 8, `render_card` error → Task 8; "markdown needs the theme" →
  Tasks 4–5; decisions 2 and 4 → Task 2 guards + Tasks 4/8; §6 contrast
  table → Task 3; §7 guards `rgb_skins_meet_wcag` (+`badge_warn`),
  `rgb_skins_carry_no_dim`, `ansi_paints_no_decorative_bg` → Tasks 2–3;
  `ansi_render_has_no_rgb` (status + markdown parts) → Tasks 4/6, panel part
  deferred to PR-3 as stated; `contrast_pairs_catch_hidden_diff` /
  `…_stripe` → Task 3. `ansi_reverse_only_status_and_focus` is PR-3's (it
  needs the panel).
- **Placeholders:** none; every edit has its old and new text.
- **Type consistency:** `Theme: Copy` (Task 2) is what lets Task 3's tests
  write `let mut t = MUR;`. `render(src, width, theme)` (Task 4) is the
  signature Task 5's `rerender_markdown` and test use. `agent_rendered(text,
  width, theme)` (Task 4) is what Task 5's test calls. `status_buffer` /
  `assert_painted` (Task 6) are not reused by Task 8, which adds its own
  `style_in`.
