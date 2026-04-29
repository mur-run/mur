//! Smoke test for the companion integration test harness (M8.1).
//!
//! Verifies that `Harness` builds successfully and that a single tick
//! completes without panicking.  The outcome (Sent or Skipped) is not
//! asserted here — that is left to the scenario-specific tests in M8.2–M8.4.

mod companion_integration_common;
use companion_integration_common::Harness;

use chrono::TimeZone;

#[tokio::test]
async fn harness_builds_and_runs_one_tick() {
    let mut h = Harness::builder()
        .at_utc(chrono::Utc.with_ymd_and_hms(2026, 4, 29, 9, 0, 0).unwrap())
        .build();
    // Must not panic; outcome may be Sent or Skipped depending on situation/schedule.
    let _ = h.tick().await;
}
