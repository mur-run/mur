//! Shared harness for companion integration tests (Phase 1.1, Spec §8.3).
//!
//! Each integration test file uses:
//!     mod companion_integration_common;
//!     use companion_integration_common::Harness;
//!
//! Cargo picks up each `.rs` file in `tests/` as its own integration-test
//! target.  Sibling test files include this module via `mod
//! companion_integration_common;` — Cargo resolves the path automatically.
//! Use `#[allow(dead_code)]` on helpers that not every test uses; otherwise
//! unused items in this file produce warnings in tests that don't reference
//! them.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use mur_agent_runtime::companion::{
    clock::{Clock, MockClock},
    notifier::{CompanionMessage, Notifier, NotifyOutcome},
    outbox::{Outbox, OutboxConfig, TickOutcome},
    picker::{BanditState, Picker, TemplateId, TemplateState},
};
use mur_agent_runtime::durable::ledger::Ledger;
use mur_agent_runtime::llm::stub::StubLlm;
use mur_agent_runtime::llm::{LlmClient, LlmError, LlmRequest, LlmResponse};
use mur_common::agent::{ActiveHours, ProactiveConfig, QuietHours};
use mur_common::companion::Situation;
use rand::rngs::StdRng;
use tempfile::TempDir;

// ─── FakeOutcome ─────────────────────────────────────────────────────────────
//
// `NotifyOutcome::Failed(anyhow::Error)` does not implement `Clone`, so we
// track the desired outcome using this small mirror enum and convert on each
// `send()` call.

/// Mirrors `NotifyOutcome` for storage inside `FakeNotifier`.
/// `NotifyOutcome::Failed` is not `Clone`, so we use a sentinel string instead.
#[derive(Clone, Debug)]
pub enum FakeOutcomeKind {
    Delivered,
    Skipped {
        reason: String,
    },
    /// Simulates a `NotifyOutcome::Failed`; the error message is the payload.
    Failed(String),
}

impl FakeOutcomeKind {
    fn into_notify_outcome(self) -> NotifyOutcome {
        match self {
            FakeOutcomeKind::Delivered => NotifyOutcome::Delivered,
            FakeOutcomeKind::Skipped { reason } => NotifyOutcome::Skipped { reason },
            FakeOutcomeKind::Failed(msg) => NotifyOutcome::Failed(anyhow::anyhow!("{}", msg)),
        }
    }
}

// ─── FakeNotifier ─────────────────────────────────────────────────────────────

/// Collects every `CompanionMessage` passed to `send()` so tests can assert on
/// the count, content, and ordering of delivered messages.
pub struct FakeNotifier {
    inner: Mutex<Vec<CompanionMessage>>,
    /// What the next `send()` call should return.  Defaults to `Delivered`.
    outcome: Mutex<FakeOutcomeKind>,
}

impl FakeNotifier {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Vec::new()),
            outcome: Mutex::new(FakeOutcomeKind::Delivered),
        })
    }

    /// Returns a clone of all messages delivered so far.
    pub fn delivered(&self) -> Vec<CompanionMessage> {
        self.inner.lock().unwrap().clone()
    }

    /// Returns the total number of `send()` calls received.
    pub fn count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Override the outcome returned by the next (and all subsequent) `send()` calls.
    pub fn set_outcome(&self, kind: FakeOutcomeKind) {
        *self.outcome.lock().unwrap() = kind;
    }
}

#[async_trait]
impl Notifier for FakeNotifier {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn send(&self, msg: &CompanionMessage) -> anyhow::Result<NotifyOutcome> {
        self.inner.lock().unwrap().push(msg.clone());
        let kind = self.outcome.lock().unwrap().clone();
        Ok(kind.into_notify_outcome())
    }
}

// ─── StubLlm helpers ──────────────────────────────────────────────────────────
//
// `mur_agent_runtime::llm::StubLlm` is the canonical deterministic LLM.
// We expose thin factory helpers for the scenarios tests commonly need.

/// Returns a `StubLlm` loaded with the default embedded scenarios (see
/// `stub_scenarios.yaml`).  Responses are in zh-TW where the situation key
/// matches; English falls back to the stub default.
pub fn stub_llm_default() -> Arc<dyn LlmClient> {
    Arc::new(StubLlm::with_default_scenarios())
}

/// Returns a `StubLlm` whose only scenario matches every request (contains "")
/// and returns a clean zh-TW body.  Passes the linter and the zh heuristic.
pub fn stub_llm_clean_zh() -> Arc<dyn LlmClient> {
    let yaml = r#"
- match: { contains: "" }
  response: "嗨，今天有什麼想分享的嗎？"
"#;
    Arc::new(StubLlm::from_yaml(yaml).expect("inline yaml must be valid"))
}

/// Returns a `StubLlm` whose only scenario matches every request and returns a
/// clean English body.  Passes the linter; does not trigger the zh heuristic.
pub fn stub_llm_clean_en() -> Arc<dyn LlmClient> {
    let yaml = r#"
- match: { contains: "" }
  response: "Hi, how are you doing today?"
"#;
    Arc::new(StubLlm::from_yaml(yaml).expect("inline yaml must be valid"))
}

/// Returns a `StubLlm` that always faults with a 429 rate-limit error.
pub fn stub_llm_force_429() -> Arc<dyn LlmClient> {
    let yaml = r#"
- match: { contains: "" }
  fault: rate_limit
"#;
    Arc::new(StubLlm::from_yaml(yaml).expect("inline yaml must be valid"))
}

/// Returns a `StubLlm` that produces an English body even when the locale is
/// zh-TW.  Useful for testing the i18n / linter rejection path.
pub fn stub_llm_force_english_for_zh() -> Arc<dyn LlmClient> {
    let yaml = r#"
- match: { contains: "" }
  response: "Hello! This is an English response."
"#;
    Arc::new(StubLlm::from_yaml(yaml).expect("inline yaml must be valid"))
}

/// Returns a `StubLlm` whose first call returns a dirty/short body (triggers
/// linter), and subsequent calls return a clean zh-TW body.
///
/// Implementation: the first scenario matches the unique string "<<DIRTY>>"
/// which the linter will reject (it's too short), and the fallback scenario
/// returns the clean body.  In practice, the outbox regenerates on linter
/// failure, and the second call hits the catch-all.
pub fn stub_llm_dirty_then_clean() -> Arc<dyn LlmClient> {
    // Both calls match the catch-all — but we use a SimpleStubLlm that counts
    // calls internally so we can return different bodies.
    Arc::new(DirtyThenCleanStub {
        calls: Mutex::new(0),
    })
}

struct DirtyThenCleanStub {
    calls: Mutex<u64>,
}

#[async_trait]
impl LlmClient for DirtyThenCleanStub {
    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let n = {
            let mut c = self.calls.lock().unwrap();
            let prev = *c;
            *c += 1;
            prev
        };
        let text = if n == 0 {
            // Too short — linter will reject (fewer than ~10 chars of content).
            "x".to_string()
        } else {
            "嗨，今天有什麼想分享的嗎？".to_string()
        };
        Ok(LlmResponse {
            text,
            input_tokens: 0,
            output_tokens: 0,
            model: "dirty-then-clean-stub".into(),
        })
    }

    fn model_name(&self) -> &str {
        "dirty-then-clean-stub"
    }
}

// ─── seed_default_bandit_state ────────────────────────────────────────────────

/// Builds a `BanditState` seeded with one template per situation that tests
/// commonly exercise (two greeting, two check-in, one quote, one link).
pub fn seed_default_bandit_state() -> BanditState {
    let mut map: BTreeMap<TemplateId, TemplateState> = BTreeMap::new();
    for (id, situation) in [
        ("greet-1", Situation::MorningGreeting),
        ("greet-2", Situation::MorningGreeting),
        ("check-1", Situation::GentleCheckIn),
        ("check-2", Situation::GentleCheckIn),
        ("quote-1", Situation::ShareQuote),
        ("link-1", Situation::ShareLink),
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

// ─── HarnessBuilder ───────────────────────────────────────────────────────────

pub struct HarnessBuilder {
    base_utc: DateTime<Utc>,
    proactive_enabled: bool,
    daily_cap: u8,
    quiet_hours: Option<QuietHours>,
    active_hours: Option<ActiveHours>,
    locale: String,
    voice_md: String,
    bandit_state: BanditState,
    llm: Arc<dyn LlmClient>,
    seed: u64,
}

impl HarnessBuilder {
    pub fn new() -> Self {
        Self {
            base_utc: Utc.with_ymd_and_hms(2026, 4, 29, 8, 0, 0).unwrap(),
            proactive_enabled: true,
            daily_cap: 3,
            quiet_hours: None,
            active_hours: None,
            locale: "en-US".into(),
            voice_md: "You are a helpful companion.\n".into(),
            bandit_state: seed_default_bandit_state(),
            llm: stub_llm_clean_en(),
            seed: 42,
        }
    }

    pub fn at_utc(mut self, ts: DateTime<Utc>) -> Self {
        self.base_utc = ts;
        self
    }

    pub fn proactive_enabled(mut self, v: bool) -> Self {
        self.proactive_enabled = v;
        self
    }

    pub fn daily_cap(mut self, n: u8) -> Self {
        self.daily_cap = n;
        self
    }

    pub fn locale(mut self, s: &str) -> Self {
        self.locale = s.into();
        self
    }

    pub fn voice_md(mut self, s: &str) -> Self {
        self.voice_md = s.into();
        self
    }

    pub fn quiet_hours(mut self, qh: QuietHours) -> Self {
        self.quiet_hours = Some(qh);
        self
    }

    pub fn active_hours(mut self, ah: ActiveHours) -> Self {
        self.active_hours = Some(ah);
        self
    }

    pub fn llm(mut self, llm: Arc<dyn LlmClient>) -> Self {
        self.llm = llm;
        self
    }

    pub fn seed(mut self, n: u64) -> Self {
        self.seed = n;
        self
    }

    pub fn bandit_state(mut self, state: BanditState) -> Self {
        self.bandit_state = state;
        self
    }

    pub fn build(self) -> Harness {
        let home = TempDir::new().unwrap();
        let clock: Arc<MockClock> = Arc::new(MockClock::at(self.base_utc));

        let ledger_dir = home.path().join("companion/outbox-ledger");
        std::fs::create_dir_all(&ledger_dir).unwrap();
        let ledger = Ledger::open(&ledger_dir).unwrap();

        let picker = Picker::with_seed(self.bandit_state, self.seed);

        let proactive = ProactiveConfig {
            enabled: self.proactive_enabled,
            learning_until: None,
            quiet_hours: self.quiet_hours,
            active_hours: self.active_hours,
            daily_cap: self.daily_cap,
            channels: vec!["fake".into()],
            paused_until: None,
        };

        let notifier = FakeNotifier::new();

        let config = OutboxConfig {
            llm: self.llm,
            notifier: notifier.clone() as Arc<dyn Notifier>,
            voice_md: self.voice_md,
            locale: self.locale,
            // M2.2.4 — these tests pre-date prompt_seed substitution; an
            // empty seed map keeps them on the legacy hardcoded prompt path.
            prompt_seeds: BTreeMap::new(),
            name_for_user: String::new(),
            first_memory: None,
            formality: String::new(),
            extra_instructions: String::new(),
            relationship: mur_common::companion::Relationship::Friend,
        };

        let clock_for_outbox: Arc<dyn Clock> = clock.clone();
        let outbox = Outbox::with_picker(clock_for_outbox, ledger, picker, proactive, config);

        Harness {
            home,
            clock,
            notifier,
            outbox,
        }
    }
}

impl Default for HarnessBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Harness ──────────────────────────────────────────────────────────────────

/// Top-level test harness for companion integration tests.
///
/// Construction via `Harness::builder().build()`.  Call `tick()` to drive one
/// outbox cycle; call `advance(d)` to move the clock forward.
pub struct Harness {
    /// Temporary home directory; lives as long as the harness.
    pub home: TempDir,
    /// Shared mock clock; call `advance()` to move time forward.
    pub clock: Arc<MockClock>,
    /// Fake notifier; call `delivered()` / `count()` to inspect sends.
    pub notifier: Arc<FakeNotifier>,
    /// The outbox under test.
    pub outbox: Outbox<StdRng>,
}

impl Harness {
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::new()
    }

    /// Run one outbox tick at the current clock time.
    pub async fn tick(&mut self) -> TickOutcome {
        let now_utc = self.clock.now_utc();
        let now_local = self.clock.now_local();
        self.outbox.run_tick(now_utc, now_local).await
    }

    /// Advance the mock clock by `d`.
    pub fn advance(&self, d: Duration) {
        self.clock.advance(d);
    }

    /// Path to the ledger directory inside the temp home.
    pub fn ledger_dir(&self) -> std::path::PathBuf {
        self.home.path().join("companion/outbox-ledger")
    }

    /// Scan the last `days` worth of ledger events, deserializing them as `E`.
    /// Returns only the successfully-deserialized entries.
    pub fn read_ledger_events<E>(&self) -> Vec<E>
    where
        E: serde::de::DeserializeOwned,
    {
        Ledger::scan_days::<E>(&self.ledger_dir(), 7)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }
}
