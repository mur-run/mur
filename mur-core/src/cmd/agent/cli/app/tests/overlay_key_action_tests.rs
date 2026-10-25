//! `overlay_key_action_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::keys::OverlayScroll;
use super::super::*;
use crossterm::event::{KeyCode, KeyModifiers};

#[test]
fn esc_closes() {
    assert_eq!(
        overlay_key_action(KeyCode::Esc, KeyModifiers::NONE),
        OverlayKeyAction::Close
    );
}

#[test]
fn enter_closes() {
    assert_eq!(
        overlay_key_action(KeyCode::Enter, KeyModifiers::NONE),
        OverlayKeyAction::Close
    );
}

#[test]
fn ctrl_d_closes_and_quits() {
    assert_eq!(
        overlay_key_action(KeyCode::Char('d'), KeyModifiers::CONTROL),
        OverlayKeyAction::CloseAndQuit
    );
}

#[test]
fn plain_d_is_ignored_not_quit() {
    assert_eq!(
        overlay_key_action(KeyCode::Char('d'), KeyModifiers::NONE),
        OverlayKeyAction::Ignore
    );
}

#[test]
fn other_chars_are_ignored_never_inserted() {
    assert_eq!(
        overlay_key_action(KeyCode::Char('x'), KeyModifiers::NONE),
        OverlayKeyAction::Ignore
    );
}

#[test]
fn scroll_keys_map_to_scroll_steps() {
    use OverlayScroll::*;
    for (code, step) in [
        (KeyCode::Up, LineUp),
        (KeyCode::Char('k'), LineUp),
        (KeyCode::Down, LineDown),
        (KeyCode::Char('j'), LineDown),
        (KeyCode::PageUp, PageUp),
        (KeyCode::Char('b'), PageUp),
        (KeyCode::PageDown, PageDown),
        (KeyCode::Char(' '), PageDown),
        (KeyCode::Home, Top),
        (KeyCode::Char('g'), Top),
        (KeyCode::End, Bottom),
        (KeyCode::Char('G'), Bottom),
    ] {
        assert_eq!(
            overlay_key_action(code, KeyModifiers::NONE),
            OverlayKeyAction::Scroll(step),
            "{code:?}"
        );
    }
}

#[test]
fn ctrl_letters_never_scroll() {
    assert_eq!(
        overlay_key_action(KeyCode::Char('k'), KeyModifiers::CONTROL),
        OverlayKeyAction::Ignore
    );
}

#[test]
fn scroll_offset_counts_up_from_bottom() {
    assert_eq!(apply_overlay_scroll(0, OverlayScroll::LineUp, 20), 1);
    assert_eq!(apply_overlay_scroll(0, OverlayScroll::LineDown, 20), 0);
    assert_eq!(apply_overlay_scroll(5, OverlayScroll::PageUp, 20), 25);
    assert_eq!(apply_overlay_scroll(5, OverlayScroll::PageDown, 20), 0);
    assert_eq!(apply_overlay_scroll(5, OverlayScroll::Top, 20), u16::MAX);
    assert_eq!(apply_overlay_scroll(500, OverlayScroll::Bottom, 20), 0);
}

#[test]
fn zero_page_still_moves_one_line() {
    // Before the first render `overlay_page` is 0; PgUp must not be a no-op.
    assert_eq!(apply_overlay_scroll(0, OverlayScroll::PageUp, 0), 1);
}

#[test]
fn ctrl_c_is_ignored_overlay_only_recognises_ctrl_d() {
    assert_eq!(
        overlay_key_action(KeyCode::Char('c'), KeyModifiers::CONTROL),
        OverlayKeyAction::Ignore
    );
}
