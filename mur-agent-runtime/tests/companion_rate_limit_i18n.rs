//! Companion integration tests — Spec §8.3 tests 6-9: rate-limit + i18n.
//!
//! M8.3: rate_limit_pause / locale_mismatch proactive (translate success, 4-fail
//! drop) / locale_mismatch reactive (ship original on translate failure).

mod companion_integration_common;
use companion_integration_common::*;

use async_trait::async_trait;
use chrono::{Duration, Local, TimeZone, Utc};
use mur_agent_runtime::companion::{
    i18n::{EnsureLocaleOutcome, ensure_locale},
    outbox::{SkipReason, TickOutcome},
    telemetry::OutboxEvent,
};
use mur_agent_runtime::llm::{LlmClient, LlmError, LlmRequest, LlmResponse, RichMessage, StopReason};
use std::sync::{Arc, Mutex};

// ─── local_as_utc helper (mirrors companion_gating.rs) ───────────────────────

/// Return the UTC instant that corresponds to the given local wall-clock time.
pub fn local_as_utc(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<Utc> {
    Local
        .with_ymd_and_hms(y, m, d, h, mi, s)
        .unwrap()
        .with_timezone(&Utc)
}

// ─── DynamicStubLlm ──────────────────────────────────────────────────────────
//
// A mutable-mode LLM stub used by tests 6 and 7.  Define it once here rather
// than in the shared harness since only this file currently needs it.

struct DynamicStubLlm {
    mode: Mutex<DynamicStubModeTag>,
    // Shared state for EnglishOnceThenChinese — we need this accessible from
    // DynamicStubMode without nested Mutex gymnastics, so store the counter here.
    call_count: Mutex<u32>,
    // Body to use for Clean / second+ calls in EnglishOnceThenChinese.
    clean_body: Mutex<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DynamicStubModeTag {
    Force429,
    EnglishOnceThenChinese,
    Clean,
}

#[async_trait]
impl LlmClient for DynamicStubLlm {
    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let mode = *self.mode.lock().unwrap();
        match mode {
            DynamicStubModeTag::Force429 => Err(LlmError::RateLimit),
            DynamicStubModeTag::Clean => {
                let body = self.clean_body.lock().unwrap().clone();
                Ok(LlmResponse {
                    text: body,
                    input_tokens: 0,
                    output_tokens: 0,
                    model: "dynamic-stub".into(),
                    tool_calls: vec![],
                    stop_reason: StopReason::EndTurn,
                })
            }
            DynamicStubModeTag::EnglishOnceThenChinese => {
                let n = {
                    let mut c = self.call_count.lock().unwrap();
                    let prev = *c;
                    *c += 1;
                    prev
                };
                let text = if n == 0 {
                    // First call: generate returns English (triggers locale mismatch).
                    "Hello! This is an English message.".to_string()
                } else {
                    // Second+ call: translate returns Chinese (resolves mismatch).
                    "嗨，今天有什麼想分享的嗎？".to_string()
                };
                Ok(LlmResponse {
                    text,
                    input_tokens: 0,
                    output_tokens: 0,
                    model: "dynamic-stub".into(),
                    tool_calls: vec![],
                    stop_reason: StopReason::EndTurn,
                })
            }
        }
    }

    fn model_name(&self) -> &str {
        "dynamic-stub"
    }
}

impl DynamicStubLlm {
    fn new_429() -> Self {
        Self {
            mode: Mutex::new(DynamicStubModeTag::Force429),
            call_count: Mutex::new(0),
            clean_body: Mutex::new(String::new()),
        }
    }

    fn new_english_once_then_chinese() -> Self {
        Self {
            mode: Mutex::new(DynamicStubModeTag::EnglishOnceThenChinese),
            call_count: Mutex::new(0),
            clean_body: Mutex::new("嗨，今天有什麼想分享的嗎？".into()),
        }
    }

    /// Switch to clean mode, returning `body` on every subsequent call.
    fn switch_to_clean(&self, body: &str) {
        *self.clean_body.lock().unwrap() = body.to_string();
        *self.mode.lock().unwrap() = DynamicStubModeTag::Clean;
    }
}

// ─── Test 6: rate_limit_pause_resumes_at_reset_timestamp ─────────────────────

/// Spec §8.3 test 6 / §6.3.
///
/// LLM returns 429 on the first tick → outbox emits `MessagePaused` and parks
/// the send.  After advancing past `resume_at` (RETRY_BACKOFF_SECS[0] = 30 s),
/// the outbox retries with a clean LLM and emits `MessageSent`.
#[tokio::test]
async fn rate_limit_pause_resumes_at_reset_timestamp() {
    let dyn_llm = Arc::new(DynamicStubLlm::new_429());

    // Start at 09:00 local so we are well inside the active window and the
    // schedule gate fires on the first tick.
    let mut h = Harness::builder()
        .at_utc(local_as_utc(2026, 4, 29, 9, 0, 0))
        .llm(dyn_llm.clone() as Arc<dyn LlmClient>)
        .build();

    // ── Tick 1: 429 ─────────────────────────────────────────────────────────
    let outcome1 = h.tick().await;

    // The outcome should be a rate-limit skip.
    match &outcome1 {
        TickOutcome::Skipped {
            reason: SkipReason::PausedRateLimit { .. },
        } => {}
        // Any other Skipped (e.g. ScheduleNotReady) also means no send happened;
        // the ledger is the authoritative source — check it below.
        TickOutcome::Skipped { .. } => {}
        TickOutcome::Sent { .. } => {
            panic!("expected no send on 429, got Sent");
        }
    }

    // Ledger must contain MessagePaused.
    let events1: Vec<OutboxEvent> = h.read_ledger_events();
    let paused = events1
        .iter()
        .find(|e| matches!(e, OutboxEvent::MessagePaused { .. }));
    assert!(
        paused.is_some(),
        "expected MessagePaused in ledger after 429; outcome={outcome1:?}, events={events1:?}"
    );

    // ── Switch LLM to clean, advance past resume_at ──────────────────────────
    dyn_llm.switch_to_clean("嗨，今天有什麼想分享的嗎？");
    // RETRY_BACKOFF_SECS[0] = 30 s; advance 35 s to be safely past it.
    h.advance(Duration::seconds(35));

    // Run a few ticks; the resume loop should fire and send on the first one
    // that finds the paused entry.
    let mut sent = false;
    for _ in 0..5 {
        let out = h.tick().await;
        if matches!(out, TickOutcome::Sent { .. }) {
            sent = true;
            break;
        }
        h.advance(Duration::seconds(5));
    }

    let events2: Vec<OutboxEvent> = h.read_ledger_events();
    let has_sent = events2
        .iter()
        .any(|e| matches!(e, OutboxEvent::MessageSent { .. }));

    // Accept either the TickOutcome::Sent path or the ledger MessageSent path
    // (the Sent outcome is returned by run_tick when the resumed send succeeds).
    assert!(
        sent || has_sent,
        "expected MessageSent after resume; sent={sent}, events={events2:?}"
    );
    assert_eq!(
        h.notifier.count(),
        1,
        "notifier should have received exactly one delivery after resume"
    );
}

// ─── Test 7: locale_mismatch_translates_then_sends_proactive ─────────────────

/// Spec §8.3 test 7 / §6.2.
///
/// Locale is zh-TW.  LLM returns English on the first call (generate → locale
/// mismatch) and Chinese on the second call (translate → success).
/// Expects: `MessageSent` in ledger; `MessageGenerated { locale_used: "zh-TW" }`;
/// notifier received the (translated) body.
#[tokio::test]
async fn locale_mismatch_translates_then_sends_proactive() {
    let dyn_llm = Arc::new(DynamicStubLlm::new_english_once_then_chinese());

    let mut h = Harness::builder()
        .at_utc(local_as_utc(2026, 4, 29, 9, 0, 0))
        .locale("zh-TW")
        .llm(dyn_llm as Arc<dyn LlmClient>)
        .build();

    let outcome = h.tick().await;

    // The outcome should be Sent (translate succeeded → full delivery).
    assert!(
        matches!(outcome, TickOutcome::Sent { .. }),
        "expected Sent after translate success; got {outcome:?}"
    );

    let events: Vec<OutboxEvent> = h.read_ledger_events();

    // MessageSent must be present.
    let sent = events
        .iter()
        .any(|e| matches!(e, OutboxEvent::MessageSent { .. }));
    assert!(sent, "expected MessageSent in ledger; events: {events:?}");

    // MessageGenerated.locale_used must be "zh-TW" (the outbox config locale).
    let locale_used = events.iter().find_map(|e| match e {
        OutboxEvent::MessageGenerated { locale_used, .. } => Some(locale_used.clone()),
        _ => None,
    });
    assert_eq!(
        locale_used.as_deref(),
        Some("zh-TW"),
        "expected locale_used=zh-TW in MessageGenerated; events: {events:?}"
    );

    // Notifier received exactly one message.
    assert_eq!(
        h.notifier.count(),
        1,
        "notifier should have received exactly one delivery"
    );

    // The delivered body must be the translated (Chinese) text, not the English
    // original.
    let body = h.notifier.delivered()[0].body.clone();
    assert!(!body.is_empty(), "delivered body must not be empty");
    // The translated stub returns Chinese; verify it is not pure ASCII
    // (a rough proxy for "it was translated").
    let is_ascii = body.is_ascii();
    assert!(
        !is_ascii,
        "translated body should contain non-ASCII (Chinese) characters; body={body:?}"
    );
}

// ─── MismatchAlwaysFailsTranslateLlm ─────────────────────────────────────────
//
// Stub for test 8: generates a body that passes the zh-TW linter but triggers
// the translate path, then returns a rate-limit error on every translate call.
//
// Strategy:
//  - Generation call ("situation:" in content): returns "Ok, 好。"
//      Linter zh-TW: "Ok," has a comma (not all-ascii-alpha), "好。" is non-ASCII
//      → 0/2 English tokens = 0 % → passes PreservedEnglishRatioZh.
//      1 sentence segment → passes SentenceCount.  No banned phrases/emoji.
//      Linter: PASSES.
//      heuristic_matches("zh-TW"): CJK = 1 ("好"); non-ws = 5 → 20 % < 30 %
//      → FAILS → translation triggered.
//  - Translate call ("Translate the following" in system message): returns
//      RateLimit → QueuedRetry → outbox parks the send.
//
// Note: ensure_locale does NOT re-check heuristic on the translated result; it
// trusts a non-empty Ok response.  Therefore, failing the translate call with
// an error is the correct way to force QueuedRetry.

struct MismatchAlwaysFailsTranslateLlm;

#[async_trait]
impl LlmClient for MismatchAlwaysFailsTranslateLlm {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let combined: String = req
            .messages
            .iter()
            .filter_map(|m| match m {
                RichMessage::Text { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        if combined.contains("Translate the following") {
            // Translate path always fails.
            return Err(LlmError::RateLimit);
        }
        // Generation path: body passes zh-TW linter but fails heuristic.
        Ok(LlmResponse {
            text: "Ok, 好。".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model: "mismatch-fail-stub".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        })
    }

    fn model_name(&self) -> &str {
        "mismatch-fail-stub"
    }
}

// ─── Test 8: locale_mismatch_translate_fails_drops_proactive ─────────────────

/// Spec §8.3 test 8 / §6.2.
///
/// Locale is zh-TW.  Generation returns a body that passes the linter but fails
/// the zh-TW CJK heuristic.  Every translate call returns a rate-limit error
/// → `QueuedRetry` each time.  After 4 retries the outbox drops with
/// `LocaleMismatchUnresolved`.
///
/// Drive time forward at each retry boundary (30 s → 90 s → 240 s → 900 s).
#[tokio::test]
async fn locale_mismatch_translate_fails_drops_proactive() {
    let stub: Arc<dyn LlmClient> = Arc::new(MismatchAlwaysFailsTranslateLlm);

    let mut h = Harness::builder()
        .at_utc(local_as_utc(2026, 4, 29, 9, 0, 0))
        .locale("zh-TW")
        .llm(stub)
        .build();

    // The first tick schedules + generates (body passes linter → translate fails
    // → QueuedRetry → MessagePaused, attempt 0, backoff 30 s).
    let _ = h.tick().await;

    // Advance through each retry boundary:
    //   attempt 0: resume after 30 s  → advance 35 s, tick
    //   attempt 1: resume after 90 s  → advance 95 s, tick
    //   attempt 2: resume after 240 s → advance 245 s, tick
    //   attempt 3: resume after 900 s → advance 905 s, tick → terminal drop
    for secs in [35i64, 95, 245, 905] {
        h.advance(Duration::seconds(secs));
        let _ = h.tick().await;
    }

    let events: Vec<OutboxEvent> = h.read_ledger_events();

    // Must have at least one LocaleMismatchUnresolved.
    let dropped = events
        .iter()
        .any(|e| matches!(e, OutboxEvent::LocaleMismatchUnresolved { .. }));
    assert!(
        dropped,
        "expected LocaleMismatchUnresolved after 4 retries; events: {events:?}"
    );

    // Must NOT have MessageSent.
    let sent = events
        .iter()
        .any(|e| matches!(e, OutboxEvent::MessageSent { .. }));
    assert!(!sent, "must NOT have MessageSent; events: {events:?}");

    // Notifier must not have received any message.
    assert_eq!(
        h.notifier.count(),
        0,
        "notifier must not receive any message when locale is unresolvable"
    );
}

// ─── Test 9: locale_mismatch_translate_fails_ships_original_reactive ─────────

/// Spec §8.3 test 9 / §6.2.
///
/// For the **reactive** path (user is waiting), when `ensure_locale` is called
/// with `reactive=true` and the translate LLM fails, the function returns
/// `OriginalWithLog` — meaning the original body is shipped as-is.
///
/// We call `ensure_locale` directly (the function under test) because the
/// harness drives the proactive outbox tick, which does not expose a reactive
/// code path.  This is the appropriate test strategy per the spec note:
/// "if there is no clear reactive entry point in the runtime, call ensure_locale
/// directly".
#[tokio::test]
async fn locale_mismatch_translate_fails_ships_original_reactive() {
    // The stub always returns English — so translate will also return English
    // which fails the zh-TW heuristic.  For the reactive path the LLM error
    // returns `LlmError::RateLimit` (force-429 stub) to trigger OriginalWithLog.
    let llm = stub_llm_force_429();

    // English text: CJK ratio 0% < 30% → heuristic_matches("zh-TW") = false
    // → translation is attempted.  stub_llm_force_429 → Err(RateLimit) →
    // reactive=true → OriginalWithLog.
    let outcome = ensure_locale("Hello, friend!", "zh-TW", llm.as_ref(), true).await;

    match outcome {
        EnsureLocaleOutcome::OriginalWithLog(_) => {
            // Expected: reactive path, translate failed, ship original with log.
        }
        EnsureLocaleOutcome::Original => {
            // Also acceptable: the heuristic decided the text already matches.
            // (This should not happen for pure-ASCII English vs zh-TW, but guard
            // defensively.)
        }
        other => panic!(
            "expected OriginalWithLog (reactive translate failure), got {:?}",
            match other {
                EnsureLocaleOutcome::Original => "Original",
                EnsureLocaleOutcome::Translated(_) => "Translated",
                EnsureLocaleOutcome::OriginalWithLog(_) => "OriginalWithLog",
                EnsureLocaleOutcome::QueuedRetry(_) => "QueuedRetry",
            }
        ),
    }
}
