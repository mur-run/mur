//! notify_message + set_unread_badge — pure functions tested without
//! a Tauri runtime via the *_inner / payload helpers.

use mur_agent_gui_lib::companion_bridge::commands::{notify_payload_for, sanitize_badge_count};

#[test]
fn notify_payload_uses_situation_as_title() {
    let payload = notify_payload_for("morning_greeting", "早安 David。");
    assert_eq!(payload.title, "morning_greeting");
    assert_eq!(payload.body, "早安 David。");
}

#[test]
fn notify_payload_truncates_body_at_200_chars() {
    let long = "a".repeat(500);
    let payload = notify_payload_for("morning_greeting", &long);
    assert_eq!(payload.body.chars().count(), 200);
    assert!(payload.body.ends_with('…'));
}

#[test]
fn badge_clamps_zero_means_clear() {
    assert_eq!(sanitize_badge_count(0), None);
}

#[test]
fn badge_caps_at_99() {
    assert_eq!(sanitize_badge_count(150), Some(99));
}

#[test]
fn badge_passes_normal_values_through() {
    assert_eq!(sanitize_badge_count(7), Some(7));
}
