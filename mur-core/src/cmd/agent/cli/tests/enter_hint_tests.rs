//! The composer hint used to say Shift+Enter unconditionally, but Shift+Enter
//! only inserts a newline when the kitty keyboard protocol was actually
//! pushed (`keyboard_enhancement_active`). On terminals without it —
//! macOS Terminal.app — Shift+Enter sends the message. These tests pin the
//! pure `active`-parameterized forms so they never touch the global flag,
//! which parallel tests would race on.

use super::super::app::{enter_hint_for, newline_fallback_notice};

#[cfg(target_os = "macos")]
const FALLBACK_CHORD: &str = "Option+Enter";
#[cfg(not(target_os = "macos"))]
const FALLBACK_CHORD: &str = "Alt+Enter";

#[test]
fn supported_terminal_hints_shift_enter() {
    for full in [true, false] {
        let hint = enter_hint_for(true, full);
        assert!(hint.contains("Shift+Enter"), "full={full}: {hint}");
    }
}

#[test]
fn unsupported_terminal_hints_the_fallback_chord_not_shift_enter() {
    for full in [true, false] {
        let hint = enter_hint_for(false, full);
        assert!(!hint.contains("Shift+Enter"), "full={full}: {hint}");
        assert!(hint.contains(FALLBACK_CHORD), "full={full}: {hint}");
    }
}

#[test]
fn startup_notice_only_when_unsupported() {
    assert_eq!(newline_fallback_notice(true), None);
    let notice = newline_fallback_notice(false).expect("notice when unsupported");
    assert!(notice.contains("Shift+Enter"), "{notice}");
    assert!(notice.contains(FALLBACK_CHORD), "{notice}");
}
