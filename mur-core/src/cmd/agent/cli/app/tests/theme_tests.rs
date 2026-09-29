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
    assert!(
        bgs.iter().all(|b| Some(*b) == MUR.surface_alt.bg),
        "{bgs:?}"
    );
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
