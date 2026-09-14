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
    /// Status bar and ordinary card background (bg only).
    pub surface: Style,
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
    /// Agent-name / AUTO badge on the status bar; the SETTLEMENT title chip.
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
/// every colour. Alias `dark`. "Muted" is DIM rather than bright-black
/// (ANSI 8): Solarized renders that slot invisible on its dark background.
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
    border_type: BorderType::Plain,
    inner_padding: 1,
    compact_input: false,
};

/// Background `light` is designed against (it does not paint one).
pub const ASSUMED_BG_LIGHT: Color = Color::Rgb(0xff, 0xff, 0xff);

/// For light terminals. Every text token is at least 4.5:1 against white,
/// body text 15.5:1 — `light_and_mur_meet_wcag` holds the line.
pub const LIGHT: Theme = Theme {
    text: rgb(0x1f, 0x24, 0x30),
    muted: rgb(0x5c, 0x63, 0x70),
    emphasis: rgb(0x00, 0x00, 0x00).add_modifier(Modifier::BOLD),
    accent: rgb(0x0b, 0x6e, 0x8f),
    accent_alt: rgb(0x1a, 0x6b, 0x3a),
    ok: rgb(0x0f, 0x6e, 0x35),
    warn: rgb(0x8a, 0x5a, 0x00),
    error: rgb(0xb3, 0x26, 0x1e),
    border: rgb(0xc9, 0xcc, 0xd6),
    surface: Style::new().bg(Color::Rgb(0xee, 0xf0, 0xf5)),
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
    border_type: BorderType::Rounded,
    inner_padding: 1,
    compact_input: false,
};

/// Background `mur` is designed against (it does not paint one).
pub const ASSUMED_BG_MUR: Color = Color::Rgb(0x0b, 0x0b, 0x1a);

/// The brand skin: purple for the operator, gold for the agent, on a deep
/// navy. Same contrast guard as `light`; the greys moved (the old secondary
/// grey read 4.6:1), the purple and gold stayed.
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
pub const CLAY: Theme = Theme {
    text: rgb(0xff, 0xff, 0xff),
    muted: rgb(0x99, 0x99, 0x99),
    emphasis: rgb(0xff, 0xff, 0xff).add_modifier(Modifier::BOLD),
    accent: rgb(0xd9, 0x77, 0x57),
    accent_alt: rgb(0xb1, 0xb9, 0xf9),
    ok: rgb(0x4e, 0xba, 0x65),
    warn: rgb(0xff, 0xc1, 0x07),
    error: rgb(0xff, 0x6b, 0x80),
    border: rgb(0x88, 0x88, 0x88),
    surface: Style::new().bg(Color::Rgb(0x26, 0x26, 0x26)),
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

    /// WCAG 2 relative luminance of an sRGB colour.
    fn luminance(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else {
            panic!("contrast is only defined for Rgb tokens, got {c:?}")
        };
        let lin = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
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
    fn rgb_skins_meet_wcag() {
        for (name, theme, bg) in [
            ("light", &LIGHT, ASSUMED_BG_LIGHT),
            ("mur", &MUR, ASSUMED_BG_MUR),
            ("clay", &CLAY, ASSUMED_BG_CLAY),
        ] {
            let settlement_bg = theme.settlement_surface.bg.unwrap_or(bg);
            for (background_name, background) in [("terminal", bg), ("settlement", settlement_bg)] {
                let r = contrast_ratio(theme.text.fg.expect("text token has a colour"), background);
                assert!(r >= 7.0, "{name}/{background_name}: text is {r:.1}:1");
                for (label, s) in [
                    ("muted", theme.muted),
                    ("emphasis", theme.emphasis),
                    ("accent", theme.accent),
                    ("accent_alt", theme.accent_alt),
                    ("ok", theme.ok),
                    ("warn", theme.warn),
                    ("error", theme.error),
                ] {
                    let r = contrast_ratio(s.fg.expect("text token has a colour"), background);
                    assert!(r >= 4.5, "{name}/{background_name}: {label} is {r:.1}:1");
                }
            }
            let badge_bg = theme.badge.bg.expect("badge has a background");
            let r = contrast_ratio(theme.badge.fg.expect("badge has a foreground"), badge_bg);
            assert!(r >= 4.5, "{name}: badge is {r:.1}:1");
        }
    }

    #[test]
    fn light_settlement_is_readable_without_terminal_background_assumptions() {
        let bg = LIGHT
            .settlement_surface
            .bg
            .expect("light settlement must paint a stable background");
        let text = LIGHT
            .settlement_text
            .fg
            .expect("light settlement copy must have a foreground");
        let ratio = contrast_ratio(text, bg);
        assert!(ratio >= 7.0, "light settlement text is only {ratio:.1}:1");
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
            ("text", ANSI.text),
            ("muted", ANSI.muted),
            ("emphasis", ANSI.emphasis),
            ("accent", ANSI.accent),
            ("accent_alt", ANSI.accent_alt),
            ("ok", ANSI.ok),
            ("warn", ANSI.warn),
            ("error", ANSI.error),
            ("border", ANSI.border),
            ("surface", ANSI.surface),
            ("badge", ANSI.badge),
        ] {
            assert!(
                named(s.fg) && named(s.bg),
                "ansi.{label} pins a colour: {s:?}"
            );
        }
    }
}
