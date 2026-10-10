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
    assert_eq!(
        esc_action(None, true, true, ReviewEsc::Detached),
        EscAction::Arm
    );
}

#[test]
fn esc_arm_when_not_streaming_and_has_text() {
    assert_eq!(
        esc_action(None, false, false, ReviewEsc::Detached),
        EscAction::Arm
    );
}

#[test]
fn esc_nothing_when_not_streaming_and_empty() {
    assert_eq!(
        esc_action(None, false, true, ReviewEsc::Detached),
        EscAction::Nothing
    );
}

#[test]
fn esc_cancel_restore_on_second_press_while_streaming() {
    assert_eq!(
        esc_action(recent(), true, true, ReviewEsc::Detached),
        EscAction::CancelAndRestore
    );
}

#[test]
fn esc_cancel_restore_on_second_press_streaming_has_text() {
    assert_eq!(
        esc_action(recent(), true, false, ReviewEsc::Detached),
        EscAction::CancelAndRestore
    );
}

#[test]
fn esc_clear_input_on_second_press_not_streaming_has_text() {
    assert_eq!(
        esc_action(recent(), false, false, ReviewEsc::Detached),
        EscAction::ClearInput
    );
}

#[test]
fn esc_nothing_on_second_press_not_streaming_empty() {
    assert_eq!(
        esc_action(recent(), false, true, ReviewEsc::Detached),
        EscAction::Nothing
    );
}

#[test]
fn esc_arm_when_window_expired_streaming() {
    assert_eq!(
        esc_action(expired(), true, true, ReviewEsc::Detached),
        EscAction::Arm
    );
}

#[test]
fn esc_arm_when_window_expired_has_text() {
    assert_eq!(
        esc_action(expired(), false, false, ReviewEsc::Detached),
        EscAction::Arm
    );
}

#[test]
fn esc_arm_at_exact_boundary() {
    assert_eq!(
        esc_action(at_boundary(), false, false, ReviewEsc::Detached),
        EscAction::Arm
    );
}

#[test]
fn esc_nothing_when_window_expired_not_streaming_empty() {
    assert_eq!(
        esc_action(expired(), false, true, ReviewEsc::Detached),
        EscAction::Nothing
    );
}

// ---- P3b-§6.1: Esc during a review session (AC-P3b-22). The `Detached`
// cases above are today's behaviour with an extra argument.

/// A turn in flight: the first press asks for a pause whatever the stream or
/// composer state, the second inside the window aborts the turn.
#[test]
fn review_turn_in_flight_first_press_requests_pause() {
    for (streaming, input_empty) in [(true, true), (true, false), (false, true), (false, false)] {
        assert_eq!(
            esc_action(None, streaming, input_empty, ReviewEsc::TurnInFlight),
            EscAction::RequestPause,
            "streaming={streaming} input_empty={input_empty}"
        );
        assert_eq!(
            esc_action(expired(), streaming, input_empty, ReviewEsc::TurnInFlight),
            EscAction::RequestPause,
            "an expired window is a first press"
        );
    }
}

#[test]
fn review_turn_in_flight_second_press_aborts_the_turn() {
    for (streaming, input_empty) in [(true, true), (true, false), (false, true), (false, false)] {
        assert_eq!(
            esc_action(recent(), streaming, input_empty, ReviewEsc::TurnInFlight),
            EscAction::AbortTurn,
            "streaming={streaming} input_empty={input_empty}"
        );
    }
}

/// Waiting at the send prompt: Esc only clears the note being typed. It is
/// never a pause (nothing is in flight) and never `CancelAndRestore`.
#[test]
fn review_awaiting_confirm_esc_only_clears_the_note() {
    assert_eq!(
        esc_action(None, false, false, ReviewEsc::AwaitingConfirm),
        EscAction::Arm
    );
    assert_eq!(
        esc_action(None, false, true, ReviewEsc::AwaitingConfirm),
        EscAction::Nothing
    );
    assert_eq!(
        esc_action(recent(), false, false, ReviewEsc::AwaitingConfirm),
        EscAction::ClearInput
    );
    assert_eq!(
        esc_action(recent(), false, true, ReviewEsc::AwaitingConfirm),
        EscAction::Nothing
    );
    for last in [None, recent(), expired()] {
        for (streaming, input_empty) in [(true, true), (true, false), (false, true), (false, false)]
        {
            let a = esc_action(last, streaming, input_empty, ReviewEsc::AwaitingConfirm);
            assert!(
                !matches!(a, EscAction::CancelAndRestore | EscAction::RequestPause),
                "{a:?} must not happen while awaiting confirm"
            );
        }
    }
}
