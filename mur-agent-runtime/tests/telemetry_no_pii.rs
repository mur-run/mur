//! Spec §7.3 / R12: PII (the user's `name_for_user`) must not appear in
//! ledger events.  Drive a 24 h MockClock simulation; assert the sentinel
//! never lands in any ledger file.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use mur_agent_runtime::companion::{
    clock::{Clock as _, MockClock},
    notifier::{CompanionMessage, Notifier, NotifyOutcome},
    outbox::{Outbox, OutboxConfig, TickOutcome},
    picker::{BanditState, Picker, TemplateState},
};
use mur_agent_runtime::durable::ledger::Ledger;
use mur_agent_runtime::llm::{LlmClient, LlmError, LlmRequest, LlmResponse};
use mur_common::agent::ProactiveConfig;
use mur_common::companion::Situation;
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;

const SENTINEL: &str = "Sentinel-User-XYZ";

// ── Fakes ─────────────────────────────────────────────────────────────────────

struct FakeLlm;

#[async_trait]
impl LlmClient for FakeLlm {
    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        // Return a body that does NOT contain the sentinel.
        Ok(LlmResponse {
            text: "Hello there.".into(),
            input_tokens: 1,
            output_tokens: 3,
            model: "fake".into(),
        })
    }
    fn model_name(&self) -> &str {
        "fake"
    }
}

struct FakeNotifier;

#[async_trait]
impl Notifier for FakeNotifier {
    fn name(&self) -> &'static str {
        "fake"
    }
    async fn send(&self, _msg: &CompanionMessage) -> Result<NotifyOutcome> {
        Ok(NotifyOutcome::Delivered)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn build_state() -> BanditState {
    let mut map: BTreeMap<String, TemplateState> = BTreeMap::new();
    for (id, situation) in [
        ("tmpl-greet", Situation::MorningGreeting),
        ("tmpl-check", Situation::GentleCheckIn),
    ] {
        map.insert(
            id.into(),
            TemplateState {
                id: id.into(),
                situation,
                weight: 1.0,
                last_used_at: None,
                pos_count: 0,
                neg_count: 0,
                dismiss_count: 0,
                cooldown_days: 0,
            },
        );
    }
    BanditState {
        version: 1,
        morning_sent_today: None,
        templates: map,
    }
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn no_pii_in_ledger_after_24h_simulation() {
    let tmp = TempDir::new().unwrap();
    let ledger_dir = tmp.path().join("ledger");

    let base_utc = Utc.with_ymd_and_hms(2026, 4, 29, 8, 0, 0).unwrap();
    let clock: Arc<MockClock> = Arc::new(MockClock::at(base_utc));

    let ledger = Ledger::open(&ledger_dir).unwrap();
    let picker = Picker::with_seed(build_state(), 7);

    let proactive = ProactiveConfig {
        enabled: true,
        learning_until: None,
        quiet_hours: None,
        active_hours: None,
        daily_cap: 5,
        channels: vec!["fake".into()],
        paused_until: None,
    };

    // voice_md WITH the sentinel — this flows into the LLM system prompt.
    // The test verifies it does NOT escape into the ledger.
    let voice_md = format!("You are a friend talking to {SENTINEL}.\n");

    let config = OutboxConfig {
        llm: Arc::new(FakeLlm) as Arc<dyn LlmClient>,
        notifier: Arc::new(FakeNotifier) as Arc<dyn Notifier>,
        voice_md,
        locale: "en-US".into(),
    };

    let mut outbox = Outbox::with_picker(clock.clone(), ledger, picker, proactive, config);

    // Drive 24 h: 1 min per tick = 1440 ticks.
    for _ in 0..1440 {
        let now_utc = clock.now_utc();
        let now_local = clock.now_local();
        let _outcome: TickOutcome = outbox.run_tick(now_utc, now_local).await;
        clock.advance(Duration::minutes(1));
    }

    // Scan every .jsonl file written into the ledger directory.
    // R12: none of them may contain the sentinel string.
    if !ledger_dir.exists() {
        // No events were written at all — trivially passes.
        return;
    }
    for entry in std::fs::read_dir(&ledger_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains(SENTINEL),
            "ledger file {} contains sentinel — voice/PII leaked into telemetry",
            path.display()
        );
    }
}
