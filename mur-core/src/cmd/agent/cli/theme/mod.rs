//! Skin/theme definitions for the agent CLI TUI: one semantic token
//! vocabulary, three palettes. Tokens are `Style`, not `Color`, because
//! `ansi` says "muted" with `DIM` where `light` says it with a grey, and a
//! paint site must not know which. See
//! `docs/superpowers/specs/2026-09-09-murmur-skin-redesign-design.md`.

// `emphasis` has no paint site until the redesign PR (markdown table
// headers take it); the token is part of the vocabulary from the start.
#![allow(dead_code)]

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

pub mod contrast;

#[derive(Clone, Copy, Debug)]
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
    // ── diff ──────────────────────────────────────────────────────────────
    /// The `▌+` / `▌-` gutter marks on an edit card's diff rows. Saturated
    /// colour lives HERE and not on the row text: a whole line painted red or
    /// green reads as an error/success banner, and long edits turn the
    /// transcript into two walls of colour.
    pub diff_add_mark: Style,
    pub diff_del_mark: Style,
    /// The wash behind a changed row. A *background* tint, never a foreground
    /// one — it says "this line moved" without recolouring a single character,
    /// so the code stays the same ink as every other line on screen. Keep
    /// these within a few points of `surface`: if the tint is loud enough to
    /// notice on its own it is too loud to read through. `ANSI` leaves them
    /// empty because it has no RGB to tint with, and an ANSI `on_red` slab is
    /// exactly the banner this avoids.
    pub diff_add_bg: Style,
    pub diff_del_bg: Style,
    /// The ink a changed row's *code* is set in. An RGB skin keeps this equal
    /// to `text` and lets the tint do the talking. A skin with no tint to
    /// spend — `ansi` — has nowhere else to put the signal, so it colours the
    /// row itself here. Exactly one of the two channels carries the change on
    /// any given skin; setting both is how a diff turns into a pair of
    /// banners.
    pub diff_add_text: Style,
    pub diff_del_text: Style,
    // ── chrome ────────────────────────────────────────────────────────────
    /// Unfocused rules: composer top rule, table grid, fleet rail.
    pub border: Style,
    /// Status bar and ordinary card background (bg only).
    pub surface: Style,
    /// Alternate-row background: the table zebra stripe (bg only). `ansi`
    /// leaves it empty — it never learns the terminal's background, and a
    /// guessed slab is what hid striped rows on light terminals.
    pub surface_alt: Style,
    /// Per-turn settlement card palette. It is complete rather than only a
    /// background: `light` may run inside a dark terminal, so inheriting its
    /// normal dark-on-light text tokens would make the card unreadable.
    pub settlement_surface: Style,
    pub settlement_text: Style,
    pub settlement_muted: Style,
    pub settlement_accent: Style,
    pub settlement_ok: Style,
    pub settlement_warn: Style,
    pub settlement_error: Style,
    /// Agent-name / READS / MONITOR chip on the status bar; the SETTLEMENT
    /// title chip.
    pub badge: Style,
    /// A status chip that means risk is on: AUTO and AUTO:<tools>.
    pub badge_warn: Style,
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

const fn bg(r: u8, g: u8, b: u8) -> Style {
    Style::new().bg(Color::Rgb(r, g, b))
}

// The four skins are `static`, not `const`, on purpose: `resolve_skin` and
// `skin_name` identify a theme by `std::ptr::eq`, and a `const` has no address
// of its own — each use site materialises a fresh temporary, so the pointer
// comparison is only ever accidentally true when the optimiser happens to
// merge them. A `static` has one address for the life of the program.

/// Follows the terminal: ANSI slots only, so the user's own theme decides
/// every colour. Alias `dark`. "Muted" is DIM rather than bright-black
/// (ANSI 8): Solarized renders that slot invisible on its dark background.
pub static ANSI: Theme = Theme {
    text: Style::new(),
    muted: Style::new().add_modifier(Modifier::DIM),
    emphasis: Style::new().add_modifier(Modifier::BOLD),
    accent: fg(Color::Cyan),
    accent_alt: fg(Color::Green),
    ok: fg(Color::Green),
    warn: fg(Color::Yellow),
    error: fg(Color::Red),
    diff_add_mark: fg(Color::Green).add_modifier(Modifier::BOLD),
    diff_del_mark: fg(Color::Red).add_modifier(Modifier::BOLD),
    // A background tint needs a background colour to sit next to, and this is
    // the one palette that never learns what the terminal's background is. The
    // 256-cube greens and maroons look right on a dark terminal and swallow
    // the text on a light one, so `ansi` spends its colour on the row's ink
    // instead: the named green/red slots, which the user's own theme has
    // already tuned to be readable against their background.
    diff_add_bg: Style::new(),
    diff_del_bg: Style::new(),
    diff_add_text: fg(Color::Green),
    diff_del_text: fg(Color::Red),
    border: Style::new().add_modifier(Modifier::DIM),
    surface: Style::new(),
    surface_alt: Style::new(),
    // Do not invert a completed turn: on light terminals that becomes a large,
    // attention-stealing white slab. The cyan rail and title chip define this
    // card while the body stays inside the terminal's own quiet surface.
    settlement_surface: Style::new(),
    settlement_text: Style::new(),
    settlement_muted: Style::new().add_modifier(Modifier::DIM),
    settlement_accent: fg(Color::Cyan),
    settlement_ok: fg(Color::Green),
    settlement_warn: fg(Color::Yellow),
    settlement_error: fg(Color::Red),
    badge: fg(Color::Cyan)
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD),
    badge_warn: fg(Color::Yellow)
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD),
    border_type: BorderType::Plain,
    inner_padding: 1,
    compact_input: false,
};

/// Background `light` is designed against (it does not paint one).
pub const ASSUMED_BG_LIGHT: Color = Color::Rgb(0xff, 0xff, 0xff);

/// For light terminals. Every text token is at least 4.5:1 against white,
/// body text 15.5:1 — `light_and_mur_meet_wcag` holds the line.
pub static LIGHT: Theme = Theme {
    text: rgb(0x1f, 0x24, 0x30),
    muted: rgb(0x5c, 0x63, 0x70),
    emphasis: rgb(0x00, 0x00, 0x00).add_modifier(Modifier::BOLD),
    accent: rgb(0x0b, 0x6e, 0x8f),
    accent_alt: rgb(0x1a, 0x6b, 0x3a),
    ok: rgb(0x0f, 0x6e, 0x35),
    warn: rgb(0x8a, 0x5a, 0x00),
    error: rgb(0xb3, 0x26, 0x1e),
    diff_add_mark: rgb(0x15, 0x6e, 0x30).add_modifier(Modifier::BOLD),
    diff_del_mark: rgb(0xcf, 0x22, 0x2e).add_modifier(Modifier::BOLD),
    diff_add_bg: bg(0xe4, 0xf2, 0xe7),
    diff_del_bg: bg(0xfc, 0xe8, 0xe8),
    // The tint carries the change here; the code keeps the page's own ink.
    diff_add_text: rgb(0x1f, 0x24, 0x30),
    diff_del_text: rgb(0x1f, 0x24, 0x30),
    border: rgb(0xc9, 0xcc, 0xd6),
    surface: Style::new().bg(Color::Rgb(0xee, 0xf0, 0xf5)),
    surface_alt: Style::new().bg(Color::Rgb(0xe9, 0xec, 0xf2)),
    // A self-contained dark inset avoids both failure modes seen in practice:
    // a bright paper-like slab and dark light-skin text disappearing into the
    // user's dark terminal background.
    settlement_surface: Style::new().bg(Color::Rgb(0x20, 0x26, 0x31)),
    settlement_text: rgb(0xf4, 0xf6, 0xfa),
    settlement_muted: rgb(0xb8, 0xc0, 0xcc),
    settlement_accent: rgb(0x65, 0xcb, 0xe8),
    settlement_ok: rgb(0x70, 0xd6, 0x9a),
    settlement_warn: rgb(0xf2, 0xc6, 0x6d),
    settlement_error: rgb(0xff, 0x9a, 0x96),
    badge: Style::new()
        .fg(Color::Rgb(0x0b, 0x6e, 0x8f))
        .bg(Color::Rgb(0xe0, 0xf0, 0xf8)),
    badge_warn: Style::new()
        .fg(Color::Rgb(0x8a, 0x5a, 0x00))
        .bg(Color::Rgb(0xfb, 0xef, 0xd5)),
    border_type: BorderType::Rounded,
    inner_padding: 1,
    compact_input: false,
};

/// Background `mur` is designed against (it does not paint one).
pub const ASSUMED_BG_MUR: Color = Color::Rgb(0x0b, 0x0b, 0x1a);

/// The brand skin: purple for the operator, gold for the agent, on a deep
/// navy. Same contrast guard as `light`; the greys moved (the old secondary
/// grey read 4.6:1), the purple and gold stayed.
pub static MUR: Theme = Theme {
    text: rgb(0xe4, 0xe4, 0xf4),
    muted: rgb(0x9a, 0x9a, 0xc4),
    emphasis: rgb(0xff, 0xff, 0xff).add_modifier(Modifier::BOLD),
    accent: rgb(0xfb, 0xbf, 0x24),
    accent_alt: rgb(0xb3, 0x9d, 0xfb),
    ok: rgb(0x7f, 0xd4, 0x8f),
    warn: rgb(0xf2, 0xc7, 0x6a),
    error: rgb(0xf2, 0x8b, 0x98),
    diff_add_mark: rgb(0x7f, 0xd4, 0x8f).add_modifier(Modifier::BOLD),
    diff_del_mark: rgb(0xf2, 0x8b, 0x98).add_modifier(Modifier::BOLD),
    diff_add_bg: bg(0x1d, 0x40, 0x30),
    diff_del_bg: bg(0x5a, 0x24, 0x37),
    diff_add_text: rgb(0xe4, 0xe4, 0xf4),
    diff_del_text: rgb(0xe4, 0xe4, 0xf4),
    border: rgb(0x3f, 0x3f, 0x78),
    surface: Style::new().bg(Color::Rgb(0x14, 0x14, 0x2c)),
    surface_alt: Style::new().bg(Color::Rgb(0x1a, 0x1a, 0x36)),
    settlement_surface: Style::new().bg(Color::Rgb(0x1d, 0x1d, 0x3a)),
    settlement_text: rgb(0xe4, 0xe4, 0xf4),
    settlement_muted: rgb(0x9a, 0x9a, 0xc4),
    settlement_accent: rgb(0xfb, 0xbf, 0x24),
    settlement_ok: rgb(0x7f, 0xd4, 0x8f),
    settlement_warn: rgb(0xf2, 0xc7, 0x6a),
    settlement_error: rgb(0xf2, 0x8b, 0x98),
    badge: Style::new()
        .fg(Color::Rgb(0xfb, 0xbf, 0x24))
        .bg(Color::Rgb(0x22, 0x1a, 0x06)),
    badge_warn: Style::new()
        .fg(Color::Rgb(0x0b, 0x0b, 0x1a))
        .bg(Color::Rgb(0xf2, 0xc7, 0x6a)),
    border_type: BorderType::Rounded,
    inner_padding: 1,
    compact_input: true,
};

/// Background `clay` is designed against (it does not paint one): a
/// typical dark terminal.
pub const ASSUMED_BG_CLAY: Color = Color::Rgb(0x1a, 0x1a, 0x1a);

/// Warm terracotta on dark: terracotta for the agent, periwinkle for the
/// operator, green / amber / rose for status, grey for metadata and rules.
/// The palette follows the familiar coding-assistant dark theme; the assumed
/// background and the surface tint are ours.
pub static CLAY: Theme = Theme {
    text: rgb(0xff, 0xff, 0xff),
    muted: rgb(0x99, 0x99, 0x99),
    emphasis: rgb(0xff, 0xff, 0xff).add_modifier(Modifier::BOLD),
    accent: rgb(0xd9, 0x77, 0x57),
    accent_alt: rgb(0xb1, 0xb9, 0xf9),
    ok: rgb(0x4e, 0xba, 0x65),
    warn: rgb(0xff, 0xc1, 0x07),
    error: rgb(0xff, 0x6b, 0x80),
    diff_add_mark: rgb(0x6b, 0xd4, 0x7f).add_modifier(Modifier::BOLD),
    diff_del_mark: rgb(0xff, 0x8c, 0x9c).add_modifier(Modifier::BOLD),
    diff_add_bg: bg(0x26, 0x48, 0x2e),
    diff_del_bg: bg(0x5e, 0x2e, 0x2e),
    diff_add_text: rgb(0xff, 0xff, 0xff),
    diff_del_text: rgb(0xff, 0xff, 0xff),
    border: rgb(0x88, 0x88, 0x88),
    surface: Style::new().bg(Color::Rgb(0x26, 0x26, 0x26)),
    surface_alt: Style::new().bg(Color::Rgb(0x2a, 0x2a, 0x2a)),
    settlement_surface: Style::new().bg(Color::Rgb(0x30, 0x27, 0x25)),
    settlement_text: rgb(0xff, 0xff, 0xff),
    settlement_muted: rgb(0xb8, 0xae, 0xaa),
    settlement_accent: rgb(0xe8, 0x91, 0x72),
    settlement_ok: rgb(0x72, 0xd2, 0x83),
    settlement_warn: rgb(0xff, 0xc9, 0x38),
    settlement_error: rgb(0xff, 0x83, 0x91),
    badge: Style::new()
        .fg(Color::Rgb(0x1a, 0x1a, 0x1a))
        .bg(Color::Rgb(0xd9, 0x77, 0x57)),
    badge_warn: Style::new()
        .fg(Color::Rgb(0x1a, 0x1a, 0x1a))
        .bg(Color::Rgb(0xff, 0xc1, 0x07)),
    border_type: BorderType::Rounded,
    inner_padding: 1,
    compact_input: false,
};

/// The names `/skin` and `--skin` accept, in the order they are listed.
/// `dark` is an alias of `ansi` and is not listed.
pub const SKIN_CHOICES: &[(&str, &str)] = &[
    ("ansi", "default — follows your terminal"),
    ("light", "light terminals"),
    ("mur", "MUR brand"),
    ("clay", "warm terracotta on dark"),
];

/// Human-readable canonical names, derived from the same registry as menus.
pub const SKIN_NAMES: &str = "ansi, light, mur, clay";

const KNOWN: [(&str, &Theme); 5] = [
    ("ansi", &ANSI),
    ("dark", &ANSI),
    ("light", &LIGHT),
    ("mur", &MUR),
    ("clay", &CLAY),
];

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

/// True if `name` is a valid skin name (the alias included).
pub fn is_known_skin(name: &str) -> bool {
    KNOWN.iter().any(|(n, _)| *n == name)
}

#[cfg(test)]
mod tests;
