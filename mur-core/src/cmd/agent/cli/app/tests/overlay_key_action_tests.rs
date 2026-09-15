//! `overlay_key_action_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

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
fn arrow_keys_are_ignored() {
    assert_eq!(
        overlay_key_action(KeyCode::Down, KeyModifiers::NONE),
        OverlayKeyAction::Ignore
    );
}

#[test]
fn ctrl_c_is_ignored_overlay_only_recognises_ctrl_d() {
    assert_eq!(
        overlay_key_action(KeyCode::Char('c'), KeyModifiers::CONTROL),
        OverlayKeyAction::Ignore
    );
}

#[test]
fn ctrl_t_is_the_monitor_shortcut_and_ctrl_m_is_never_bound() {
    // ^M is Enter on every terminal; binding it would shadow submit.
    use crate::cmd::agent::cli::events::binds_ctrl;
    assert!(!binds_ctrl('m'), "Ctrl+M must never be bound");
    assert!(binds_ctrl('t'));
}
