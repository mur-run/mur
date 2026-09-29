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
        c.complete(
            CallOutcome::Failed,
            String::new(),
            false,
            0,
            Some("boom".into()),
            19,
        );
        let lines = card_lines(&c, theme, false, 80);
        assert_eq!(style_in(&lines, "edit_file").fg, theme.error.fg, "header");
        assert_eq!(style_in(&lines, "boom").fg, theme.error.fg, "error line");
    }
}
