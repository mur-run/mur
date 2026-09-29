//! The markdown renderer paints only with the active skin (murmur UI spec
//! §1): stripe `surface_alt`, grid and rule `border`, quote `muted`, list
//! marker and table header `accent`.

use super::{RULE, render};
use crate::cmd::agent::cli::theme::{ANSI, CLAY, LIGHT, MUR};
use ratatui::style::{Color, Style};
use ratatui::text::{Span, Text};

const DOC: &str =
    "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n| 5 | 6 |\n\n> quoted\n\n---\n\n- item\n";
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

/// Inline and fenced code take the skin's `code` ink: a hard-coded terminal
/// yellow washed out on `light`'s white page.
#[test]
fn code_uses_the_skin_code_token() {
    for theme in [&ANSI, &LIGHT, &MUR, &CLAY] {
        let t = render("see `inline` here\n\n```\nfenced\n```\n", WIDTH, theme);
        for word in ["inline", "fenced"] {
            let (_, style) = span(&t, |s| s.content.contains(word));
            assert_eq!(style.fg, theme.code.fg, "{word}");
        }
    }
}

/// The stripe stays inside the frame: a painted outer `│` fills its whole
/// cell, so half a cell of stripe showed past the table's edge.
#[test]
fn stripe_leaves_the_outer_borders_unpainted() {
    for theme in [&LIGHT, &MUR, &CLAY] {
        let t = render(DOC, WIDTH, theme);
        let striped: Vec<_> = t
            .lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.style.bg.is_some()))
            .collect();
        assert!(!striped.is_empty(), "no striped row");
        for l in striped {
            let (first, last) = (l.spans.first().unwrap(), l.spans.last().unwrap());
            assert_eq!(first.content, "│");
            assert_eq!(last.content, "│");
            assert!(first.style.bg.is_none(), "left border painted");
            assert!(last.style.bg.is_none(), "right border painted");
        }
    }
}
