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
/// every colour. Alias `dark`. (The greys are still today's RGB values in
/// this PR; the redesign PR replaces them with `Reset` + modifiers.)
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
        assert!(std::ptr::eq(resolve_skin(""), &ANSI));
        assert!(!is_known_skin("neon"));
        assert!(!is_known_skin("DARK"));
    }

    #[test]
    fn light_and_mur_resolve_to_themselves() {
        assert!(std::ptr::eq(resolve_skin("light"), &LIGHT));
        assert!(std::ptr::eq(resolve_skin("mur"), &MUR));
        assert_eq!(skin_name(&LIGHT), "light");
        assert_eq!(skin_name(&MUR), "mur");
    }
}
