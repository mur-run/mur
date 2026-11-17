//! #2442: an ESC-cancelled turn that produced nothing is withdrawn from the
//! transcript, because its text goes back to the composer.

use super::super::*;

fn sent(a: &mut App, text: &str) {
    a.push_system("earlier notice");
    a.begin_user_turn(text).expect("turn starts");
    a.finish_partial();
}

#[test]
fn withdraws_bubble_and_placeholder_when_nothing_ran() {
    let mut a = App::test_fixture();
    sent(&mut a, "hello");
    assert!(a.withdraw_cancelled_turn());
    assert!(
        !a.messages.iter().any(|m| m.role == Role::User),
        "user bubble withdrawn"
    );
    assert!(
        !a.messages.iter().any(|m| m.text == "(cancelled)"),
        "cancelled placeholder withdrawn"
    );
    assert!(a.messages.iter().any(|m| m.text == "earlier notice"));
}

#[test]
fn keeps_turn_that_produced_output() {
    let mut a = App::test_fixture();
    sent(&mut a, "hello");
    a.turn_produced_output = true;
    assert!(!a.withdraw_cancelled_turn());
    assert!(a.messages.iter().any(|m| m.role == Role::User));
}

#[test]
fn rewinds_flush_cursor_when_bubble_already_in_scrollback() {
    let mut a = App::test_fixture();
    sent(&mut a, "hello");
    // Inline mode flushes the settled bubble while the reply streams.
    a.flushed_upto = a.messages.len() - 1;
    a.wants_screen_wipe = false;
    assert!(a.withdraw_cancelled_turn());
    assert_eq!(a.flushed_upto, 0, "replay from the top");
    assert!(a.wants_screen_wipe, "scrollback copy must be purged");
}

#[test]
fn no_wipe_when_bubble_still_in_live_band() {
    let mut a = App::test_fixture();
    sent(&mut a, "hello");
    let before = a.flushed_upto;
    a.wants_screen_wipe = false;
    assert!(a.withdraw_cancelled_turn());
    assert!(!a.wants_screen_wipe);
    assert_eq!(a.flushed_upto, before.min(a.messages.len()));
}
