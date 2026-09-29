//! Palette guards, moved out of `theme.rs` for CLAUDE.md's 800-line rule.
//! Pure movement: dedented one level, nothing else.

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

fn contrast_ratio(a: Color, b: Color) -> f64 {
    super::contrast::ratio(a, b).expect("contrast is only defined for Rgb tokens")
}

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
        ("surface_alt", ANSI.surface_alt),
        ("badge_warn", ANSI.badge_warn),
        ("settlement_surface", ANSI.settlement_surface),
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
    for (name, t) in [
        ("ansi", &ANSI),
        ("light", &LIGHT),
        ("mur", &MUR),
        ("clay", &CLAY),
    ] {
        assert!(
            t.surface_alt.fg.is_none(),
            "{name}.surface_alt sets a foreground"
        );
        assert!(
            t.badge_warn.fg.is_some(),
            "{name}.badge_warn has no foreground"
        );
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
