//! Outbox tests, split by milestone:
//!   - `rhythm`  — M5.3 (proactive disabled, daily cap, morning greeting, day rollover)
//!   - `lint`    — M5.4 (linter pass / regenerate / persistent / notifier failure)
//!   - `i18n`    — M5.5 (translate / 429 backoff / 4-failure drop / generate rate-limit pause)
//!   - `dismiss` — M5.6 (passive-dismiss sweep)

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use anyhow::Result as AnyhowResult;
use async_trait::async_trait;
use chrono::{Local, NaiveDate, NaiveTime, TimeZone, Utc};
use mur_common::agent::{ProactiveConfig, QuietHours};
use mur_common::companion::Situation;
use rand::RngCore;
use tempfile::TempDir;

use crate::companion::clock::{Clock, MockClock};
use crate::companion::notifier::{CompanionMessage, Notifier, NotifyOutcome};
use crate::companion::picker::{BanditState, Picker, TemplateState};
use crate::companion::telemetry::OutboxEvent;
use crate::durable::ledger::Ledger;
use crate::llm::LlmClient;
use crate::llm::stub::StubLlm;

use super::{Outbox, OutboxConfig, SkipReason, TickOutcome};
use crate::companion::earned_permission::BlockReason;

mod dismiss;
mod i18n;
mod lint;
mod rhythm;

// ── FakeNotifier ─────────────────────────────────────────────────────────

/// In-test notifier that records every delivered message and can be
/// configured to return a fixed outcome.
struct FakeNotifier {
    outcome: NotifyOutcomeKind,
    calls: Mutex<Vec<CompanionMessage>>,
}

enum NotifyOutcomeKind {
    Delivered,
    #[allow(dead_code)]
    Skipped(String),
    Failed,
}

impl FakeNotifier {
    fn delivered() -> Self {
        Self {
            outcome: NotifyOutcomeKind::Delivered,
            calls: Mutex::new(Vec::new()),
        }
    }
    fn failed() -> Self {
        Self {
            outcome: NotifyOutcomeKind::Failed,
            calls: Mutex::new(Vec::new()),
        }
    }
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    fn last_body(&self) -> Option<String> {
        self.calls.lock().unwrap().last().map(|m| m.body.clone())
    }
}

#[async_trait]
impl Notifier for FakeNotifier {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn send(&self, msg: &CompanionMessage) -> AnyhowResult<NotifyOutcome> {
        self.calls.lock().unwrap().push(msg.clone());
        Ok(match &self.outcome {
            NotifyOutcomeKind::Delivered => NotifyOutcome::Delivered,
            NotifyOutcomeKind::Skipped(r) => NotifyOutcome::Skipped { reason: r.clone() },
            NotifyOutcomeKind::Failed => {
                NotifyOutcome::Failed(anyhow::anyhow!("fake notifier failure"))
            }
        })
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

/// Build a UTC `DateTime` that, when converted to the host's local timezone,
/// yields the specified `(year, month, day, hour, minute, second)`.
fn local_as_utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<Utc> {
    let naive = NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_time(NaiveTime::from_hms_opt(h, mi, s).unwrap());
    Local
        .from_local_datetime(&naive)
        .single()
        .expect("ambiguous local time in test — adjust h/m")
        .with_timezone(&Utc)
}

/// Build a `BanditState` with one template per situation.
fn seed_bandit_state() -> BanditState {
    let mut state = BanditState::default();
    for (id, situation) in [
        ("tmpl-morning", Situation::MorningGreeting),
        ("tmpl-checkin", Situation::GentleCheckIn),
        ("tmpl-quote", Situation::ShareQuote),
        ("tmpl-link", Situation::ShareLink),
    ] {
        state.templates.insert(
            id.to_string(),
            TemplateState {
                id: id.to_string(),
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
    state
}

/// Count `MessageScheduled` events in the ledger.
fn count_scheduled(base_dir: &std::path::Path) -> usize {
    Ledger::scan_days::<OutboxEvent>(base_dir, 30)
        .into_iter()
        .filter_map(|r| r.ok())
        .filter(|e| matches!(e, OutboxEvent::MessageScheduled { .. }))
        .count()
}

/// Collect all events from the ledger.
fn all_events(base_dir: &std::path::Path) -> Vec<OutboxEvent> {
    Ledger::scan_days::<OutboxEvent>(base_dir, 30)
        .into_iter()
        .filter_map(|r| r.ok())
        .collect()
}

/// Build a `ProactiveConfig` with the given params.
fn make_proactive(
    enabled: bool,
    daily_cap: u8,
    quiet_start: Option<&str>,
    quiet_end: Option<&str>,
) -> ProactiveConfig {
    ProactiveConfig {
        enabled,
        daily_cap,
        quiet_hours: quiet_start.zip(quiet_end).map(|(s, e)| QuietHours {
            start: s.to_string(),
            end: e.to_string(),
        }),
        ..ProactiveConfig::default()
    }
}

/// Build a clean-body StubLlm via YAML scenarios.
fn stub_clean_zh() -> Arc<dyn LlmClient> {
    Arc::new(
        StubLlm::from_yaml(
            r#"
- match:
    contains: "situation:"
  response: "早安。今天想從哪一件小事開始？"
"#,
        )
        .unwrap(),
    )
}

/// StubLlm that returns banned body on first call, clean on regenerate.
///
/// The outbox appends `"\n[regenerate]"` to the user prompt on its retry,
/// so we match on that suffix for the second scenario.
fn stub_bad_then_clean_zh() -> Arc<dyn LlmClient> {
    Arc::new(
        StubLlm::from_yaml(
            r#"
- match:
    contains: "[regenerate]"
  response: "早安。今天想從哪一件小事開始？"
- match:
    contains: "situation:"
  response: "今天好棒！"
"#,
        )
        .unwrap(),
    )
}

/// StubLlm that always returns a banned body.
fn stub_always_banned_zh() -> Arc<dyn LlmClient> {
    Arc::new(
        StubLlm::from_yaml(
            r#"
- match:
    contains: "situation:"
  response: "今天好棒！"
"#,
        )
        .unwrap(),
    )
}

/// Build a minimal outbox for tests.
fn make_outbox<R: RngCore + Send>(
    clock: Arc<MockClock>,
    ledger: Ledger,
    picker: Picker<R>,
    proactive: ProactiveConfig,
    llm: Arc<dyn LlmClient>,
    notifier: Arc<dyn Notifier>,
) -> Outbox<R> {
    Outbox::with_picker(
        clock,
        ledger,
        picker,
        proactive,
        OutboxConfig {
            llm,
            notifier,
            voice_md: "You are a warm companion.".to_string(),
            locale: "zh-TW".to_string(),
            prompt_seeds: BTreeMap::new(),
            name_for_user: String::new(),
            first_memory: None,
            formality: String::new(),
            extra_instructions: String::new(),
            relationship: mur_common::companion::Relationship::Friend,
        },
    )
}
