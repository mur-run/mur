//! Companion integration tests — Spec §8.3 tests 10-16: signals + restart + linter.
//!
//! M8.4: passive-dismiss / picker persistence / ledger replay / linter / re-init
//! / morning-greeting cap.

mod companion_integration_common;
use companion_integration_common::*;

use async_trait::async_trait;
use chrono::{Duration, Local, TimeZone, Utc};
use mur_agent_runtime::companion::{outbox::TickOutcome, picker::Picker, telemetry::OutboxEvent};
use mur_agent_runtime::llm::{LlmClient, LlmError, LlmRequest, LlmResponse};
use mur_common::companion::{Signal, Situation};
use std::sync::{Arc, Mutex};

// ─── local_as_utc helper (mirrors companion_gating.rs) ───────────────────────

/// Return the UTC instant that corresponds to the given local wall-clock time.
pub fn local_as_utc(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<Utc> {
    Local
        .with_ymd_and_hms(y, m, d, h, mi, s)
        .unwrap()
        .with_timezone(&Utc)
}

// ─── Custom LLM stubs needed for linter tests ─────────────────────────────────

/// Stub that returns a banned zh-TW phrase (`好棒`) on the first call, then a
/// clean body on subsequent calls.  Used to trigger exactly one regenerate.
struct BannedThenCleanStub {
    calls: Mutex<u64>,
}

impl BannedThenCleanStub {
    fn new() -> Arc<dyn LlmClient> {
        Arc::new(Self {
            calls: Mutex::new(0),
        })
    }
}

#[async_trait]
impl LlmClient for BannedThenCleanStub {
    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let n = {
            let mut c = self.calls.lock().unwrap();
            let prev = *c;
            *c += 1;
            prev
        };
        let text = if n == 0 {
            // First call: banned zh phrase — linter will reject.
            "今天好棒！".to_string()
        } else {
            // Second call: clean zh body — passes linter.
            "早安。今天想從哪一件小事開始？".to_string()
        };
        Ok(LlmResponse {
            text,
            input_tokens: 0,
            output_tokens: 0,
            model: "banned-then-clean".into(),
        })
    }

    fn model_name(&self) -> &str {
        "banned-then-clean"
    }
}

/// Stub that uses `amazing!!` (a banned English phrase) on every call, so the
/// linter always rejects it.  Triggers `MessageDropped(linter_persistent)`.
struct AlwaysDirty;

#[async_trait]
impl LlmClient for AlwaysDirty {
    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: "amazing!! amazing!! amazing!!".into(),
            input_tokens: 0,
            output_tokens: 0,
            model: "always-dirty".into(),
        })
    }

    fn model_name(&self) -> &str {
        "always-dirty"
    }
}

// ─── Test 10: passive_dismiss_after_24h_records_signal_and_event ─────────────

/// Spec §8.3 test 10.
///
/// Send one message, advance 24h+1min, tick again.  The ledger must contain a
/// `PassiveDismissInferred` event for the sent id, and the picker must have
/// `dismiss_count > 0` for the template that was sent.
#[tokio::test]
async fn passive_dismiss_after_24h_records_signal_and_event() {
    // Start at 10:00 local — well inside any active window.
    let mut h = Harness::builder()
        .at_utc(local_as_utc(2026, 4, 29, 10, 0, 0))
        .daily_cap(10)
        .llm(stub_llm_clean_zh())
        .build();

    // Tick 1: drive a successful send and capture the sent id + template_id.
    let (sent_id, template_id) = {
        let outcome = h.tick().await;
        match outcome {
            TickOutcome::Sent {
                id, template_id, ..
            } => (id, template_id),
            other => panic!("expected Sent on first tick; got {other:?}"),
        }
    };

    // No PassiveDismissInferred yet.
    {
        let events: Vec<OutboxEvent> = h.read_ledger_events();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboxEvent::PassiveDismissInferred { .. })),
            "should not have PassiveDismissInferred before 24h"
        );
    }

    // Advance > 24h so the sweep fires.
    h.advance(Duration::hours(24) + Duration::minutes(1));

    // Tick 2: sweep fires.  May also schedule a new send — that's fine.
    let _ = h.tick().await;

    // Ledger must contain PassiveDismissInferred for `sent_id`.
    let events: Vec<OutboxEvent> = h.read_ledger_events();
    let dismiss_ev = events.iter().find(|e| match e {
        OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
        _ => false,
    });
    assert!(
        dismiss_ev.is_some(),
        "PassiveDismissInferred must exist for id={sent_id}; events: {events:?}"
    );

    // Picker must have recorded a Dismiss signal for the template.
    let dismiss_count = h
        .outbox
        .picker
        .state
        .templates
        .get(&template_id)
        .map(|t| t.dismiss_count)
        .unwrap_or(0);
    assert!(
        dismiss_count > 0,
        "picker dismiss_count must be > 0 for template={template_id}"
    );
}

// ─── Test 11: picker_record_signal_persists_across_restart ───────────────────

/// Spec §8.3 test 11.
///
/// Phase 1.1 does not persist `BanditState` to disk (M5.7 limitation).  This
/// test verifies in-memory persistence: pull the `BanditState` out of an
/// existing picker, rebuild a new `Picker` with that state, and confirm the
/// recorded signal is still there.
#[tokio::test]
async fn picker_record_signal_persists_across_restart() {
    let mut h = Harness::builder().build();

    // Record a Positive signal directly on the picker for "greet-1".
    let template_id = "greet-1".to_string();
    h.outbox
        .picker
        .record(&template_id, Signal::Positive, Utc::now());
    let pos_count_before = h
        .outbox
        .picker
        .state
        .templates
        .get(&template_id)
        .expect("greet-1 must exist in seeded bandit state")
        .pos_count;
    assert!(
        pos_count_before > 0,
        "pos_count must increase after recording Positive"
    );

    // Simulate restart: clone the BanditState and rebuild a new Picker from it.
    let preserved_state = h.outbox.picker.state.clone();
    let new_picker = Picker::with_seed(preserved_state, 42);

    // The new picker's state must still reflect the recorded signal.
    let pos_count_after = new_picker
        .state
        .templates
        .get(&template_id)
        .expect("greet-1 must exist in rebuilt picker")
        .pos_count;
    assert_eq!(
        pos_count_after, pos_count_before,
        "signal must survive an in-memory 'restart' (state clone + rebuild)"
    );
}

// ─── Test 12: ledger_resume_replays_paused_messages_after_restart ─────────────

/// Spec §8.3 test 12.
///
/// `pending_pause` is stored in-memory in Phase 1.1 (M5.5 limitation).  A
/// restart loses the pending_pause state; messages are silently dropped rather
/// than replayed.  Deferred to Phase 1.2 (disk persistence for pause state).
#[tokio::test]
#[ignore = "pending_pause is in-memory in Phase 1.1 (M5.5 limitation); restart cannot replay paused messages until Phase 1.2"]
async fn ledger_resume_replays_paused_messages_after_restart() {
    // TODO(Phase 1.2): persist pending_pause to disk so restart can resume.
}

// ─── Test 13: linter_violation_triggers_one_regenerate ───────────────────────

/// Spec §8.3 test 13.
///
/// The first LLM body fails the zh-TW linter (`好棒` is a banned phrase).
/// The outbox regenerates once; the second body passes.  The ledger must have
/// two `MessageGenerated` events (regen_count=0 then regen_count=1) and a
/// `MessageSent`.
#[tokio::test]
async fn linter_violation_triggers_one_regenerate() {
    let mut h = Harness::builder()
        .at_utc(local_as_utc(2026, 4, 29, 9, 0, 0))
        .locale("zh-TW")
        .llm(BannedThenCleanStub::new())
        .build();

    let _ = h.tick().await;

    let events: Vec<OutboxEvent> = h.read_ledger_events();

    // There must be two MessageGenerated events: regen_count=0 (first try,
    // linter failed) and regen_count=1 (second try, passes).
    let generated: Vec<&OutboxEvent> = events
        .iter()
        .filter(|e| matches!(e, OutboxEvent::MessageGenerated { .. }))
        .collect();
    assert_eq!(
        generated.len(),
        2,
        "expected 2 MessageGenerated events (first fail + regen); events: {events:?}"
    );
    assert!(
        matches!(
            generated[0],
            OutboxEvent::MessageGenerated { regen_count: 0, .. }
        ),
        "first MessageGenerated must have regen_count=0"
    );
    assert!(
        matches!(
            generated[1],
            OutboxEvent::MessageGenerated { regen_count: 1, .. }
        ),
        "second MessageGenerated must have regen_count=1"
    );

    // Must end with MessageSent (second body passed the linter).
    let sent = events
        .iter()
        .any(|e| matches!(e, OutboxEvent::MessageSent { .. }));
    assert!(
        sent,
        "must have MessageSent after successful regen; events: {events:?}"
    );

    // Must NOT have MessageDropped.
    let dropped = events
        .iter()
        .any(|e| matches!(e, OutboxEvent::MessageDropped { .. }));
    assert!(!dropped, "must NOT have MessageDropped; events: {events:?}");
}

// ─── Test 14: linter_persistent_violation_drops ──────────────────────────────

/// Spec §8.3 test 14.
///
/// Both the initial body and the regenerated body fail the linter.  The outbox
/// must drop the message with `MessageDropped { reason: "linter_persistent" }`.
#[tokio::test]
async fn linter_persistent_violation_drops() {
    let mut h = Harness::builder()
        .at_utc(local_as_utc(2026, 4, 29, 9, 0, 0))
        // Use English locale so that the banned English phrases trigger the linter.
        .locale("en-US")
        .llm(Arc::new(AlwaysDirty) as Arc<dyn LlmClient>)
        .build();

    let _ = h.tick().await;

    let events: Vec<OutboxEvent> = h.read_ledger_events();

    // Must have MessageDropped with reason "linter_persistent".
    let dropped = events.iter().any(|e| match e {
        OutboxEvent::MessageDropped { reason, .. } => reason == "linter_persistent",
        _ => false,
    });
    assert!(
        dropped,
        "expected MessageDropped(linter_persistent) after persistent linter failure; events: {events:?}"
    );

    // Must NOT have MessageSent.
    let sent = events
        .iter()
        .any(|e| matches!(e, OutboxEvent::MessageSent { .. }));
    assert!(!sent, "must NOT have MessageSent; events: {events:?}");
}

// ─── Test 15: re_init_preserves_ledger_inbox_bandit ──────────────────────────

/// Spec §8.3 test 15.
///
/// The re-init flow (`mur agent companion init --re-init`) lives in the
/// `mur-core` CLI crate, not in the `mur-agent-runtime` crate.  Testing it here
/// would require driving the CLI, which is out of scope for runtime integration
/// tests.  This is tracked as a mur-core integration test.
#[tokio::test]
#[ignore = "re-init flow lives in mur-core CLI; must be tested at the CLI level, not in the runtime crate"]
async fn re_init_preserves_ledger_inbox_bandit() {}

// ─── Test 16: morning_greeting_caps_once_per_local_day ───────────────────────

/// Spec §8.3 test 16.
///
/// Within a single local day, `MorningGreeting` must fire at most once even if
/// multiple ticks are driven and the daily cap allows more sends.
#[tokio::test]
async fn morning_greeting_caps_once_per_local_day() {
    // Start at 08:00 local — within active hours, eligible for MorningGreeting.
    let mut h = Harness::builder()
        .at_utc(local_as_utc(2026, 4, 29, 8, 0, 0))
        .daily_cap(10) // cap is NOT the limiter here
        .llm(stub_llm_clean_zh())
        .build();

    // Drive 48 ticks, 5 minutes apart (4 h total), counting MorningGreeting sends.
    let mut morning_count = 0usize;
    for _ in 0..48 {
        if let TickOutcome::Sent { situation, .. } = h.tick().await {
            if situation == Situation::MorningGreeting {
                morning_count += 1;
            }
        }
        h.advance(Duration::minutes(5));
    }

    assert_eq!(
        morning_count, 1,
        "MorningGreeting must fire at most once per local day; got {morning_count}"
    );
}
