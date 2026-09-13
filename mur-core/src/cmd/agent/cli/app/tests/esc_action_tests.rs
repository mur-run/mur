//! `esc_action_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;
use std::time::{Duration, Instant};

fn recent() -> Option<Instant> {
    Some(Instant::now() - Duration::from_millis(100))
}

fn expired() -> Option<Instant> {
    Some(Instant::now() - Duration::from_millis(600))
}

fn at_boundary() -> Option<Instant> {
    Some(Instant::now() - ESC_DOUBLE_WINDOW)
}

#[test]
fn esc_arm_when_streaming_and_empty_input() {
    assert_eq!(esc_action(None, true, true), EscAction::Arm);
}

#[test]
fn esc_arm_when_not_streaming_and_has_text() {
    assert_eq!(esc_action(None, false, false), EscAction::Arm);
}

#[test]
fn esc_nothing_when_not_streaming_and_empty() {
    assert_eq!(esc_action(None, false, true), EscAction::Nothing);
}

#[test]
fn esc_cancel_restore_on_second_press_while_streaming() {
    assert_eq!(
        esc_action(recent(), true, true),
        EscAction::CancelAndRestore
    );
}

#[test]
fn esc_cancel_restore_on_second_press_streaming_has_text() {
    assert_eq!(
        esc_action(recent(), true, false),
        EscAction::CancelAndRestore
    );
}

#[test]
fn esc_clear_input_on_second_press_not_streaming_has_text() {
    assert_eq!(esc_action(recent(), false, false), EscAction::ClearInput);
}

#[test]
fn esc_nothing_on_second_press_not_streaming_empty() {
    assert_eq!(esc_action(recent(), false, true), EscAction::Nothing);
}

#[test]
fn esc_arm_when_window_expired_streaming() {
    assert_eq!(esc_action(expired(), true, true), EscAction::Arm);
}

#[test]
fn esc_arm_when_window_expired_has_text() {
    assert_eq!(esc_action(expired(), false, false), EscAction::Arm);
}

#[test]
fn esc_arm_at_exact_boundary() {
    assert_eq!(esc_action(at_boundary(), false, false), EscAction::Arm);
}

#[test]
fn esc_nothing_when_window_expired_not_streaming_empty() {
    assert_eq!(esc_action(expired(), false, true), EscAction::Nothing);
}
