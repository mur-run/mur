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
        (
            "diff_add_text",
            theme.diff_add_text,
            "diff_add_bg",
            theme.diff_add_bg,
            MIN_TEXT,
        ),
        (
            "diff_del_text",
            theme.diff_del_text,
            "diff_del_bg",
            theme.diff_del_bg,
            MIN_TEXT,
        ),
        (
            "diff_add_mark",
            theme.diff_add_mark,
            "diff_add_bg",
            theme.diff_add_bg,
            MIN_GLYPH,
        ),
        (
            "diff_del_mark",
            theme.diff_del_mark,
            "diff_del_bg",
            theme.diff_del_bg,
            MIN_GLYPH,
        ),
        (
            "settlement_text",
            theme.settlement_text,
            "settlement_surface",
            settle,
            MIN_BODY,
        ),
        (
            "settlement_muted",
            theme.settlement_muted,
            "settlement_surface",
            settle,
            MIN_TEXT,
        ),
        (
            "settlement_accent",
            theme.settlement_accent,
            "settlement_surface",
            settle,
            MIN_TEXT,
        ),
        (
            "settlement_ok",
            theme.settlement_ok,
            "settlement_surface",
            settle,
            MIN_TEXT,
        ),
        (
            "settlement_warn",
            theme.settlement_warn,
            "settlement_surface",
            settle,
            MIN_TEXT,
        ),
        (
            "settlement_error",
            theme.settlement_error,
            "settlement_surface",
            settle,
            MIN_TEXT,
        ),
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
        assert!(
            pairs(&f).contains(&("diff_add_text", "diff_add_bg")),
            "{f:?}"
        );
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
