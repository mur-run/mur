//! M4.4: Deterministic-interval should_send_now (Spec §4.7).

use chrono::{Datelike, Local, TimeZone, Timelike};
use mur_agent_runtime::companion::schedule::*;
use mur_common::agent::QuietHours;

fn at(hour: u32, minute: u32) -> chrono::DateTime<chrono::Local> {
    Local.with_ymd_and_hms(2026, 4, 29, hour, minute, 0).unwrap()
}

#[test]
fn first_send_immediate_when_under_budget() {
    let now = at(9, 0);
    let end = at(22, 0); // 13h active window remaining
    let d = should_send_now(now, None, end, 3, 0, 0);
    assert!(d.should_send);
    // 13h * 60min / 3 = 260min interval; jitter=0 → fires immediately because elapsed=∞
    assert_eq!(d.desired_interval_minutes, 260);
    assert!(d.minutes_since_last_send.is_none());
}

#[test]
fn second_send_blocked_until_interval_elapses() {
    let now = at(13, 0);
    let last = at(9, 0); // 4h ago = 240min
    let end = at(22, 0); // 9h * 60 = 540 min remaining
    // budget_remaining = 3 - 1 = 2, desired = 540/2 = 270min
    let d = should_send_now(now, Some(last), end, 3, 1, 0);
    assert_eq!(d.desired_interval_minutes, 270);
    assert_eq!(d.minutes_since_last_send, Some(240));
    assert!(!d.should_send); // 240 < 270
}

#[test]
fn second_send_allowed_when_interval_satisfied_with_jitter() {
    let now = at(13, 0);
    let last = at(9, 0); // 240 min ago
    let end = at(22, 0);
    // desired = 270min; jitter=30min → threshold = 270-30=240; elapsed=240 → eligible
    let d = should_send_now(now, Some(last), end, 3, 1, 30);
    assert!(d.should_send);
}

#[test]
fn daily_cap_blocks() {
    let now = at(15, 0);
    let last = at(9, 0);
    let end = at(22, 0);
    let d = should_send_now(now, Some(last), end, 3, 3, 0); // sent_today >= cap
    assert!(!d.should_send);
}

#[test]
fn after_window_end_blocks() {
    let now = at(23, 0);
    let end = at(22, 0);
    let d = should_send_now(now, None, end, 3, 0, 0);
    assert!(!d.should_send);
}

#[test]
fn divisor_zero_safe_when_budget_one() {
    let now = at(21, 50);
    let end = at(22, 0);
    // budget_remaining = max(1, 3-3) = 1, but cap blocks first; this test ensures no divide-by-zero
    // with budget_remaining=1: desired = 10 / 1 = 10 min
    let d = should_send_now(now, None, end, 1, 0, 0);
    assert_eq!(d.desired_interval_minutes, 10);
}

#[test]
fn parse_hhmm_works() {
    assert_eq!(parse_hhmm("22:00"), chrono::NaiveTime::from_hms_opt(22, 0, 0));
    assert_eq!(parse_hhmm("08:30"), chrono::NaiveTime::from_hms_opt(8, 30, 0));
    assert!(parse_hhmm("nope").is_none());
}

#[test]
fn active_window_end_uses_quiet_start() {
    let now = at(9, 0);
    let qh = QuietHours {
        start: "22:00".into(),
        end: "08:00".into(),
    };
    let end = active_window_end_for_today(now, Some(&qh)).unwrap();
    assert_eq!(end.hour(), 22);
    assert_eq!(end.minute(), 0);
    assert_eq!(end.day(), now.day());

    // No quiet_hours → use end of day (23:59:59 today)
    let end2 = active_window_end_for_today(now, None).unwrap();
    assert_eq!(end2.day(), now.day());
    assert_eq!(end2.hour(), 23);
}
