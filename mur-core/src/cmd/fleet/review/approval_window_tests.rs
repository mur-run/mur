//! Tests for `approval_window.rs` (#1762).

use std::time::{Duration, Instant};

use super::*;

#[test]
fn label_counts_minutes_and_rounds_seconds_up() {
    assert_eq!(remaining_label(Duration::from_secs(300)), "5m 00s");
    assert_eq!(remaining_label(Duration::from_millis(299_100)), "5m 00s");
    assert_eq!(remaining_label(Duration::from_secs(61)), "1m 01s");
    assert_eq!(remaining_label(Duration::from_secs(60)), "1m 00s");
    assert_eq!(remaining_label(Duration::from_secs(59)), "59s");
    // An open window never reads 0s.
    assert_eq!(remaining_label(Duration::from_millis(1)), "1s");
}

#[test]
fn deadline_comes_from_timeout_ms() {
    let now = Instant::now();
    let d = deadline_from(&serde_json::json!({"timeout_ms": 300_000}), now).unwrap();
    assert_eq!(d.duration_since(now), Duration::from_secs(300));
}

#[test]
fn no_usable_timeout_means_no_deadline() {
    let now = Instant::now();
    assert!(deadline_from(&serde_json::json!({}), now).is_none());
    assert!(deadline_from(&serde_json::json!({"timeout_ms": "300"}), now).is_none());
    assert_eq!(prompt_clock(None, now), "");
}

#[test]
fn clock_is_evaluated_at_ask_time() {
    let start = Instant::now();
    let d = start + Duration::from_secs(300);
    assert_eq!(prompt_clock(Some(d), start), "(5m 00s left) ");
    // A re-ask a minute later shows what is left then.
    let later = start + Duration::from_secs(60);
    assert_eq!(prompt_clock(Some(d), later), "(4m 00s left) ");
}

#[test]
fn closed_window_says_so() {
    let start = Instant::now();
    let d = start + Duration::from_secs(10);
    let closed = "(window closed — a late answer is rejected) ";
    assert_eq!(prompt_clock(Some(d), d), closed, "exactly at the deadline");
    assert_eq!(prompt_clock(Some(d), d + Duration::from_secs(5)), closed);
}
