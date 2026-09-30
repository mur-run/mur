//! Model output must never reach the terminal as a control character.
//!
//! ratatui 0.29 only drops zero-width symbols, and unicode-width 0.2 counts a
//! control character inside a string as width 1, so an ESC in a reply became
//! its own cell and crossterm printed it verbatim (`Print(cell.symbol())`).
//! A reply could then clear the screen, move the cursor, and paint a fake
//! approval card over the real one. Both paint paths are covered: the live
//! band (`Terminal::draw`) and the flush to scrollback (`insert_before`).

use super::super::render;
use super::flush_finished;
use crate::cmd::agent::cli::app::{App, ChatMsg, RenderMode, Role};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::{Terminal, TerminalOptions, Viewport};

/// ESC-CSI clear + cursor home, BEL, a raw C1 CSI (U+009B), CR, DEL.
const HOSTILE: &str = "ok\u{1b}[2J\u{1b}[1;1Hfake\u{7} \u{9b}31mred\r over\u{7f}";

fn control_cells(buf: &Buffer) -> Vec<String> {
    buf.content
        .iter()
        .map(|c| c.symbol())
        .filter(|s| s.chars().any(char::is_control))
        .map(|s| format!("{s:?}"))
        .collect()
}

fn text_of(buf: &Buffer) -> String {
    buf.content.iter().map(|c| c.symbol()).collect()
}

#[test]
fn live_band_never_holds_a_control_character() {
    let mut app = App::test_fixture();
    app.messages.push(ChatMsg::for_test(Role::User, "hi"));
    app.messages.push(ChatMsg::for_test(Role::Agent, HOSTILE));
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| render(f, &mut app)).unwrap();
    let buf = term.backend().buffer();
    let bad = control_cells(buf);
    assert!(bad.is_empty(), "control cells reached the screen: {bad:?}");
    // Negative control: scrubbing must not eat the printable text around it.
    let t = text_of(buf);
    assert!(
        t.contains("fake") && t.contains("red"),
        "printable text lost: {t:?}"
    );
}

#[test]
fn scrollback_flush_never_holds_a_control_character() {
    const VIEWPORT: u16 = 8;
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = 80;
    let mut term = Terminal::with_options(
        TestBackend::new(80, 16),
        TerminalOptions {
            viewport: Viewport::Inline(VIEWPORT),
        },
    )
    .unwrap();
    for i in 0..12 {
        app.messages
            .push(ChatMsg::for_test(Role::User, &format!("q{i}")));
        app.messages.push(ChatMsg::for_test(Role::Agent, HOSTILE));
    }
    flush_finished(&mut term, &mut app, VIEWPORT).unwrap();
    term.draw(|f| render(f, &mut app)).unwrap();
    let sb = term.backend().scrollback();
    // Guard: the flush path must actually have run, or this test proves nothing.
    assert!(sb.area.height > 0, "nothing reached scrollback");
    for buf in [sb, term.backend().buffer()] {
        let bad = control_cells(buf);
        assert!(
            bad.is_empty(),
            "control cells reached the terminal: {bad:?}"
        );
    }
    assert!(
        text_of(sb).contains("fake"),
        "printable text lost from scrollback"
    );
}
