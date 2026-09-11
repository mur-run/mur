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
    fn light_and_mur_meet_wcag() {
        for (name, theme, bg) in [
            ("light", &LIGHT, ASSUMED_BG_LIGHT),
            ("mur", &MUR, ASSUMED_BG_MUR),
        ] {
            let fg = |s: Style| s.fg.expect("text token has a colour");
            let r = contrast_ratio(fg(theme.text), bg);
            assert!(r >= 7.0, "{name}: text is {r:.1}:1");
            for (label, s) in [
                ("muted", theme.muted),
                ("emphasis", theme.emphasis),
                ("accent", theme.accent),
                ("accent_alt", theme.accent_alt),
                ("ok", theme.ok),
                ("warn", theme.warn),
                ("error", theme.error),
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
