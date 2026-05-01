//! Companion integration tests — Spec §8.3 tests 1-5: gating behaviour.
//!
//! M8.2: onboarding / disabled / daily-cap / quiet-hours / paused-until.

mod companion_integration_common;
use companion_integration_common::*;

use chrono::{Duration, Local, TimeZone, Utc};
use mur_agent_runtime::companion::{
    clock::Clock,
    earned_permission::BlockReason,
    outbox::{SkipReason, TickOutcome},
    telemetry::OutboxEvent,
};
use mur_common::agent::QuietHours;

// ─── helper: local midnight expressed as UTC ─────────────────────────────────

/// Return the UTC instant that corresponds to the given local wall-clock time.
/// This is the approach the unit tests in outbox.rs use so that quiet-hours
/// comparisons (which run in local time) work correctly on any host timezone.
pub fn local_as_utc(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<Utc> {
    Local
        .with_ymd_and_hms(y, m, d, h, mi, s)
        .unwrap()
        .with_timezone(&Utc)
}

// ─── Test 1: onboarding_writes_voice_md_and_starts_disabled ──────────────────

/// Spec §8.3 test 1.
///
/// Verifies two things:
/// 1. `compose_in_memory` produces a non-empty voice-md body (proxy for the
///    onboarding "write voice.md" step, which lives in mur-core CLI and cannot
///    be driven from this runtime-crate test).
/// 2. When `proactive.enabled = false`, 24 simulated ticks produce zero sends.
#[tokio::test]
async fn onboarding_writes_voice_md_and_starts_disabled() {
    use mur_agent_runtime::companion::voice::{VoiceInput, compose_in_memory};
    use mur_common::companion::Relationship;

    let voice = compose_in_memory(VoiceInput {
        relationship: Relationship::Friend,
        locale: "en-US",
        name_for_user: "tester",
        first_memory: None,
        formality: "polite",
        extra_instructions: "",
    });
    assert!(
        !voice.is_empty(),
        "compose_in_memory must yield a non-empty body"
    );

    let mut h = Harness::builder().proactive_enabled(false).build();
    // 24 hourly ticks — proactive is disabled so nothing should be sent.
    for _ in 0..24 {
        let _ = h.tick().await;
        h.advance(Duration::hours(1));
    }
    assert_eq!(
        h.notifier.count(),
        0,
        "proactive disabled must produce zero sends"
    );
}

// ─── Test 2: proactive_disabled_no_sends_after_24h_simulated ─────────────────

/// Spec §8.3 test 2 / acceptance criterion A3.
///
/// Drives 1 440 one-minute ticks (24 h) with proactive disabled and asserts:
/// - The notifier received 0 messages.
/// - The durable ledger contains 0 `MessageSent` events.
#[tokio::test]
async fn proactive_disabled_no_sends_after_24h_simulated() {
    let mut h = Harness::builder().proactive_enabled(false).build();

    for _ in 0..1440 {
        let _ = h.tick().await;
        h.advance(Duration::minutes(1));
    }

    assert_eq!(
        h.notifier.count(),
        0,
        "notifier should receive zero messages"
    );

    let events: Vec<OutboxEvent> = h.read_ledger_events();
    let sent_count = events
        .iter()
        .filter(|e| matches!(e, OutboxEvent::MessageSent { .. }))
        .count();
    assert_eq!(
        sent_count, 0,
        "ledger should contain zero MessageSent events"
    );
}

// ─── Test 3: proactive_enabled_respects_daily_cap ────────────────────────────

/// Spec §8.3 test 3 / acceptance criterion A4.
///
/// 24 h simulation with daily_cap=3 and a 12h active window (08:00-20:00 local).
/// Expects exactly 3 sends.
///
/// **Host-timezone note**: Quiet hours are evaluated in local time; the harness
/// advances a UTC mock clock that converts to local via `with_timezone(&Local)`.
/// Setting `at_utc(local_as_utc(…, 0, 0, 0))` means the clock starts at local
/// midnight, so the active window 08:00-20:00 local aligns correctly on any host.
#[tokio::test]
async fn proactive_enabled_respects_daily_cap() {
    // Start at local midnight so the 08:00-20:00 window is predictable.
    let base = local_as_utc(2026, 4, 29, 0, 0, 0);

    let mut h = Harness::builder()
        .at_utc(base)
        .proactive_enabled(true)
        .daily_cap(3)
        // Quiet window 20:00-08:00 → active window 08:00-20:00 (12 h).
        .quiet_hours(QuietHours {
            start: "20:00".into(),
            end: "08:00".into(),
        })
        .build();

    // 1440 one-minute ticks = 24 h.
    for _ in 0..1440 {
        let _ = h.tick().await;
        h.advance(Duration::minutes(1));
    }

    let events: Vec<OutboxEvent> = h.read_ledger_events();
    let sent_count = events
        .iter()
        .filter(|e| matches!(e, OutboxEvent::MessageSent { .. }))
        .count();

    // The cap is 3; the active window is 12h/24h so there is plenty of room.
    assert_eq!(
        sent_count, 3,
        "expected exactly 3 MessageSent events (daily cap=3); got {sent_count}. Events: {events:?}"
    );
    assert_eq!(
        h.notifier.count(),
        3,
        "notifier must record exactly 3 deliveries"
    );
}

// ─── Test 4: quiet_hours_blocks_send_in_window ───────────────────────────────

/// Spec §8.3 test 4.
///
/// Sets the clock to 23:00 local time (inside the 22:00-07:00 quiet window)
/// and asserts the first tick returns `GateBlocked(QuietHours)`.
#[tokio::test]
async fn quiet_hours_blocks_send_in_window() {
    // 23:00 local is inside the 22:00-07:00 quiet window.
    let at = local_as_utc(2026, 4, 29, 23, 0, 0);

    let mut h = Harness::builder()
        .at_utc(at)
        .quiet_hours(QuietHours {
            start: "22:00".into(),
            end: "07:00".into(),
        })
        .build();

    let outcome = h.tick().await;
    match outcome {
        TickOutcome::Skipped {
            reason: SkipReason::GateBlocked(BlockReason::QuietHours),
        } => {}
        other => panic!("expected QuietHours skip, got {other:?}"),
    }
    assert_eq!(
        h.notifier.count(),
        0,
        "no message must be delivered during quiet hours"
    );
}

// ─── Test 5: paused_until_blocks_until_expiry ────────────────────────────────

/// Spec §8.3 test 5.
///
/// Sets `proactive.paused_until = now + 1h` and verifies:
/// - The first tick is blocked with `GateBlocked(Paused)`.
/// - After advancing 2h (past the expiry), the tick is NOT blocked by `Paused`
///   (may still be skipped by schedule / situation gates).
#[tokio::test]
async fn paused_until_blocks_until_expiry() {
    // Start inside the default active window so schedule gates don't interfere
    // with the pause-expiry assertion.
    let base = local_as_utc(2026, 4, 29, 10, 0, 0);

    let mut h = Harness::builder()
        .at_utc(base)
        .proactive_enabled(true)
        .build();

    // Apply paused_until = now + 1h directly on the outbox's proactive config.
    let now = h.clock.now_utc();
    h.outbox.proactive.paused_until = Some(now + Duration::hours(1));

    // ── First tick: must be blocked by Paused. ────────────────────────────────
    let outcome = h.tick().await;
    match outcome {
        TickOutcome::Skipped {
            reason: SkipReason::GateBlocked(BlockReason::Paused),
        } => {}
        other => panic!("expected Paused skip before expiry, got {other:?}"),
    }

    // ── Advance 2h — pause has expired. ──────────────────────────────────────
    h.advance(Duration::hours(2));
    let outcome2 = h.tick().await;
    // Any other outcome (Sent, ScheduleNotReady, NoSituation, etc.) is fine.
    assert!(
        !matches!(
            outcome2,
            TickOutcome::Skipped {
                reason: SkipReason::GateBlocked(BlockReason::Paused),
            }
        ),
        "should NOT be blocked by Paused after expiry, got {outcome2:?}"
    );
}
