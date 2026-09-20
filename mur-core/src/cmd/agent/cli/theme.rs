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
            // Each surface is judged against the tokens actually painted on
            // it: the settlement card never draws with the terminal-surface
            // tokens, so checking those against its background is meaningless.
            let check = |surface: &str, bg: Color, text: Style, tokens: &[(&str, Style)]| {
                let r = contrast_ratio(text.fg.expect("text token has a colour"), bg);
                assert!(r >= 7.0, "{name}/{surface}: text is {r:.1}:1");
                for (label, s) in tokens {
                    let r = contrast_ratio(s.fg.expect("text token has a colour"), bg);
                    assert!(r >= 4.5, "{name}/{surface}: {label} is {r:.1}:1");
                }
            };
            check(
                "terminal",
                bg,
                theme.text,
                &[
                    ("muted", theme.muted),
                    ("emphasis", theme.emphasis),
                    ("accent", theme.accent),
                    ("accent_alt", theme.accent_alt),
                    ("ok", theme.ok),
                    ("warn", theme.warn),
                    ("error", theme.error),
                ],
            );
            check(
                "settlement",
                theme.settlement_surface.bg.unwrap_or(bg),
                theme.settlement_text,
                &[
                    ("muted", theme.settlement_muted),
                    ("accent", theme.settlement_accent),
                    ("ok", theme.settlement_ok),
                    ("warn", theme.settlement_warn),
                    ("error", theme.settlement_error),
                ],
            );
            let badge_bg = theme.badge.bg.expect("badge has a background");
            let r = contrast_ratio(theme.badge.fg.expect("badge has a foreground"), badge_bg);
            assert!(r >= 4.5, "{name}: badge is {r:.1}:1");
        }
    }

    /// A diff row's tint is a *wash*, not a highlight. Two things must hold on
    /// every RGB skin: the body text still clears 7:1 when it sits on the
    /// tint instead of the bare background, and the tint itself stays within
    /// 1.9:1 of that background — past that it stops reading as "this line
    /// changed" and starts reading as a coloured slab, which is the banner
    /// look this whole treatment exists to avoid.
    ///
    /// The ceiling was 1.35 and that was too timid: on a near-black terminal
    /// a tint that close to the background is invisible under any ambient
    /// light, which defeats the point of having one.
    #[test]
    fn diff_tints_are_a_wash_not_a_highlight() {
        for (name, theme, bg) in [
            ("light", &LIGHT, ASSUMED_BG_LIGHT),
            ("mur", &MUR, ASSUMED_BG_MUR),
            ("clay", &CLAY, ASSUMED_BG_CLAY),
        ] {
            let text = theme.text.fg.expect("text token has a colour");
            for (label, tint) in [("add", theme.diff_add_bg), ("del", theme.diff_del_bg)] {
                let tint = tint.bg.expect("rgb skin tints its diff rows");

                let r = contrast_ratio(text, tint);
                assert!(r >= 7.0, "{name}/{label}: text on tint is only {r:.1}:1");

                let loud = contrast_ratio(tint, bg);
                assert!(
                    loud <= 1.9,
                    "{name}/{label}: tint is {loud:.2}:1 against the background — that is a slab, not a wash"
                );
            }
        }
    }

    /// The gutter mark still has to be legible once it is painted ON the
    /// tint rather than on the terminal background — it is the only thing
    /// carrying the +/- sign in colour.
    #[test]
    fn diff_marks_read_against_their_own_tint() {
        for (name, theme) in [("light", &LIGHT), ("mur", &MUR), ("clay", &CLAY)] {
            for (label, mark, tint) in [
                ("add", theme.diff_add_mark, theme.diff_add_bg),
                ("del", theme.diff_del_mark, theme.diff_del_bg),
            ] {
                let r = contrast_ratio(
                    mark.fg.expect("mark has a colour"),
                    tint.bg.expect("rgb skin tints its diff rows"),
                );
                assert!(r >= 4.5, "{name}/{label}: mark on tint is only {r:.1}:1");
            }
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

    /// `ansi` pins no *foreground*: every text colour is a named ANSI slot or
    /// Reset, so the terminal's own theme is what the user sees. Nothing in
    /// this palette pins a background either — see the diff tokens below for
    /// why it inks its changed rows instead of tinting them.
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
            ("diff_add_mark", ANSI.diff_add_mark),
            ("diff_del_mark", ANSI.diff_del_mark),
            ("diff_add_text", ANSI.diff_add_text),
            ("diff_del_text", ANSI.diff_del_text),
        ] {
            assert!(
                named(s.fg) && named(s.bg),
                "ansi.{label} pins a colour: {s:?}"
            );
        }
    }

    /// The `ansi` diff rows carry their change in the row's *ink*, using the
    /// named green/red slots — never a background tint and never RGB. This
    /// palette cannot see the terminal's background, so a tint that reads on
    /// a dark terminal hides the text on a light one; the user's own theme has
    /// already made the named slots legible against whatever they run.
    #[test]
    fn ansi_diff_rows_are_inked_not_tinted() {
        for (label, tint) in [("add", ANSI.diff_add_bg), ("del", ANSI.diff_del_bg)] {
            assert!(
                tint == Style::new(),
                "ansi.diff_{label}_bg must stay empty, got {tint:?}"
            );
        }
        for (label, ink, want) in [
            ("add", ANSI.diff_add_text, Color::Green),
            ("del", ANSI.diff_del_text, Color::Red),
        ] {
            assert_eq!(
                ink.fg,
                Some(want),
                "ansi.diff_{label}_text must use the named {want:?} slot"
            );
            assert!(
                ink.bg.is_none(),
                "ansi.diff_{label}_text must not pin a background"
            );
        }
    }

    /// The two channels are exclusive by design: a skin either tints the row
    /// or inks it. Doing both is what makes a diff read as a stack of
    /// error/success banners — the exact look this treatment avoids.
    #[test]
    fn no_skin_both_tints_and_inks_a_diff_row() {
        for (name, theme) in [
            ("ansi", &ANSI),
            ("light", &LIGHT),
            ("mur", &MUR),
            ("clay", &CLAY),
        ] {
            for (label, tint, ink) in [
                ("add", theme.diff_add_bg, theme.diff_add_text),
                ("del", theme.diff_del_bg, theme.diff_del_text),
            ] {
                let tinted = tint.bg.is_some();
                let inked = ink.fg != theme.text.fg;
                assert!(
                    tinted != inked,
                    "{name}/{label}: tinted={tinted} inked={inked} — exactly one channel must carry the change"
                );
            }
        }
    }
}
