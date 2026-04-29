//! Outbox tick loop — Spec §4.8 steps 1, 4, 5, 6, 7, 8, 9, 11, 12.
//!
//! ## Rationale for struct shape
//!
//! `Outbox` holds:
//! - `proactive`: a snapshot of the user's `ProactiveConfig`; callers are
//!   expected to rebuild the `Outbox` (or call a future `set_proactive()`) when
//!   the config changes — this keeps `run_tick` synchronous and lock-free.
//! - `picker`: owned directly (no `Arc`) because `Picker` is not shared across
//!   threads in Phase 1.1; M5.4 will persist its state, not restructure ownership.
//! - `last_send_at` / `sent_today` / `morning_sent_today` / `today_date`: in-memory
//!   rhythm state.  They are reset on day rollover at the top of every `run_tick`.
//!   Persistence is deferred to a later milestone.
//! - `ledger`: owned; appended to in steps 7, 8, 11, 12.
//! - `clock`: `Arc<dyn Clock>` so tests can inject a `MockClock`.
//! - `llm`: `Arc<dyn LlmClient>` for message generation (step 8 — M5.4).
//! - `notifier`: `Arc<dyn Notifier>` for delivery (step 11 — M5.4).
//! - `voice_md`: pre-composed system prompt for the LLM (supplied by the
//!   supervisor in M5.7; a plain string in tests).
//!
//! ## Locale resolution (step 8)
//!
//! The locale is taken from `proactive.locale` if that field exists — but
//! `ProactiveConfig` does not carry a `locale` field in the current schema.
//! For M5.4 we therefore fall back to the `locale` field carried in the
//! `Outbox` struct itself, which callers set from `CompanionConfig.locale`
//! (the field closest to the spec's "agent_profile.locale").

use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDate, Utc};
use mur_common::companion::{Signal, Situation};
use rand::{RngCore, rngs::StdRng};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::companion::{
    clock::Clock,
    earned_permission::{self, BlockReason, GateOutcome},
    linter,
    notifier::{CompanionMessage, NotifyOutcome, Notifier},
    picker::{Picker, TemplateId},
    schedule::{self, ScheduleDecision},
    situations,
    telemetry::OutboxEvent,
};
use crate::durable::ledger::Ledger;
use crate::llm::{LlmClient, LlmError, LlmMessage, LlmRequest};
use mur_common::agent::ProactiveConfig;

// ──────────────────────────────────────────────────────────────────────────────
// Public outcome types
// ──────────────────────────────────────────────────────────────────────────────

/// Returned by `Outbox::run_tick` so callers and tests can observe what happened
/// without reaching into struct internals.
#[derive(Debug, Clone, PartialEq)]
pub enum TickOutcome {
    /// The tick was a no-op; `reason` explains which gate/check stopped it.
    Skipped { reason: SkipReason },
    /// A message was fully generated, delivered, and recorded.
    Sent {
        id: String,
        situation: Situation,
        template_id: TemplateId,
    },
}

/// Why a tick produced no `MessageSent` event.
#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// `earned_permission::check` returned `Blocked`.
    GateBlocked(BlockReason),
    /// `active_window_end_for_today` returned `None`, or `now` is already past it.
    ActiveWindowEnded,
    /// `schedule::should_send_now` returned `should_send = false`.
    ScheduleNotReady,
    /// `situations::pick_for_hour` returned `None` (all weights zero / quiet hour).
    NoSituation,
    /// `picker::Picker::pick` returned `None` (all templates on cooldown).
    NoTemplate,
    /// LLM returned a rate-limit error (M5.5 will implement pause/resume).
    LlmRateLimit,
    /// Linter failed on both the original and regenerated body.
    LinterPersistent,
    /// Notifier returned `Failed`.
    NotifierFailed,
    /// Notifier returned `Skipped`.
    NotifierSkipped,
}

// ──────────────────────────────────────────────────────────────────────────────
// OutboxConfig — bundles construction-time dependencies to keep arg count ≤ 7
// ──────────────────────────────────────────────────────────────────────────────

/// Construction-time configuration for [`Outbox`].
///
/// Separates the "what to do" knobs (LLM, notifier, voice prompt, locale) from
/// the rhythm state that changes tick-by-tick.
pub struct OutboxConfig {
    /// LLM client for message generation (step 8 — M5.4).
    pub llm: Arc<dyn LlmClient>,
    /// Delivery channel (step 11 — M5.4).
    pub notifier: Arc<dyn Notifier>,
    /// Pre-composed voice system prompt; passed by the supervisor (M5.7).
    pub voice_md: String,
    /// BCP-47 locale used as the LLM generation locale.
    /// Taken from `CompanionConfig.locale`; `ProactiveConfig` does not carry
    /// a locale field in the current schema.
    pub locale: String,
}

// ──────────────────────────────────────────────────────────────────────────────
// Outbox
// ──────────────────────────────────────────────────────────────────────────────

/// Core outbox state machine.  Call `run_tick(now_utc, now_local)` from a
/// supervising loop (typically every 60 s in production).
pub struct Outbox<R: RngCore + Send = StdRng> {
    /// Clock abstraction — `SystemClock` in production, `MockClock` in tests.
    pub clock: Arc<dyn Clock>,
    /// Append-only JSONL ledger.
    pub ledger: Ledger,
    /// Bandit-state template picker (owns its own RNG).
    pub picker: Picker<R>,
    /// Snapshot of the user's proactive config; used every tick.
    pub proactive: ProactiveConfig,
    /// LLM client for message generation (step 8 — M5.4).
    pub llm: Arc<dyn LlmClient>,
    /// Delivery channel (step 11 — M5.4).
    pub notifier: Arc<dyn Notifier>,
    /// Pre-composed voice system prompt; passed by the supervisor (M5.7).
    /// For tests, a plain string (possibly empty) is fine.
    pub voice_md: String,
    /// BCP-47 locale used as the LLM generation locale.
    /// Comes from `CompanionConfig.locale` (or caller's choice); ProactiveConfig
    /// does not carry a locale field in the current schema.
    pub locale: String,

    // ── rhythm state ──
    /// Local time of the last successfully sent message (updated at step 12).
    pub last_send_at: Option<DateTime<Local>>,
    /// Number of messages already sent today (reset on day rollover).
    pub sent_today: u8,
    /// The local date on which a `MorningGreeting` was last sent.
    /// `None` means not yet today.
    pub morning_sent_today: Option<NaiveDate>,
    /// Local date we last saw — used to detect day rollovers.
    pub today_date: NaiveDate,
}

impl Outbox<StdRng> {
    /// Production constructor — seeds picker from entropy.
    pub fn new(
        clock: Arc<dyn Clock>,
        ledger: Ledger,
        picker: Picker<StdRng>,
        proactive: ProactiveConfig,
        config: OutboxConfig,
    ) -> Self {
        let today = clock.now_local().date_naive();
        Self {
            clock,
            ledger,
            picker,
            proactive,
            llm: config.llm,
            notifier: config.notifier,
            voice_md: config.voice_md,
            locale: config.locale,
            last_send_at: None,
            sent_today: 0,
            morning_sent_today: None,
            today_date: today,
        }
    }
}

impl<R: RngCore + Send> Outbox<R> {
    /// Test constructor — caller supplies their own RNG-typed picker.
    pub fn with_picker(
        clock: Arc<dyn Clock>,
        ledger: Ledger,
        picker: Picker<R>,
        proactive: ProactiveConfig,
        config: OutboxConfig,
    ) -> Self {
        let today = clock.now_local().date_naive();
        Self {
            clock,
            ledger,
            picker,
            proactive,
            llm: config.llm,
            notifier: config.notifier,
            voice_md: config.voice_md,
            locale: config.locale,
            last_send_at: None,
            sent_today: 0,
            morning_sent_today: None,
            today_date: today,
        }
    }

    /// Execute one tick of the outbox loop.
    ///
    /// Steps implemented: **1, 4, 5, 6, 7, 8, 9, 11, 12**.
    /// Steps 2/3 (resume-paused / passive-dismiss) → M5.5 / M5.6.
    /// Step 10 (i18n locale-mismatch loop) → M5.5.
    pub async fn run_tick(
        &mut self,
        now_utc: DateTime<Utc>,
        now_local: DateTime<Local>,
    ) -> TickOutcome {
        // ── Day rollover ─────────────────────────────────────────────────────
        let today = now_local.date_naive();
        if today != self.today_date {
            self.sent_today = 0;
            self.morning_sent_today = None;
            self.today_date = today;
        }

        // ── Step 1: earned_permission gate ───────────────────────────────────
        match earned_permission::check(&self.proactive, now_utc, now_local) {
            GateOutcome::Blocked { reason } => {
                return TickOutcome::Skipped {
                    reason: SkipReason::GateBlocked(reason),
                };
            }
            GateOutcome::Allowed => {}
        }

        // ── (Steps 2 + 3 skipped — M5.5 / M5.6) ────────────────────────────

        // ── Step 4: should_send_new ──────────────────────────────────────────
        let window_end = match schedule::active_window_end_for_today(
            now_local,
            self.proactive.quiet_hours.as_ref(),
        ) {
            Some(w) => w,
            None => {
                return TickOutcome::Skipped {
                    reason: SkipReason::ActiveWindowEnded,
                };
            }
        };
        if now_local >= window_end {
            return TickOutcome::Skipped {
                reason: SkipReason::ActiveWindowEnded,
            };
        }

        // Jitter: derive a small deterministic value from the current minute so
        // we don't always fire on the exact threshold.  Range 0..=10.
        let jitter = (now_local.timestamp() % 11) as u8;

        let ScheduleDecision { should_send, .. } = schedule::should_send_now(
            now_local,
            self.last_send_at,
            window_end,
            self.proactive.daily_cap,
            self.sent_today,
            jitter,
        );
        if !should_send {
            return TickOutcome::Skipped {
                reason: SkipReason::ScheduleNotReady,
            };
        }

        // ── Step 5: pick situation ───────────────────────────────────────────
        let Some(situation) =
            situations::pick_for_hour(now_local, self.morning_sent_today, &mut self.picker.rng)
        else {
            return TickOutcome::Skipped {
                reason: SkipReason::NoSituation,
            };
        };

        // ── Step 6: pick template ────────────────────────────────────────────
        let Some(template_id) = self.picker.pick(situation.clone(), now_utc) else {
            return TickOutcome::Skipped {
                reason: SkipReason::NoTemplate,
            };
        };

        // ── Step 7: schedule — append MessageScheduled to ledger ────────────
        let id = Uuid::new_v4().to_string();
        let event = OutboxEvent::MessageScheduled {
            id: id.clone(),
            situation: situation.clone(),
            template_id: template_id.clone(),
            scheduled_for: now_utc,
        };
        if let Err(e) = self.ledger.append(&event) {
            tracing::error!("outbox: ledger append failed: {e}");
            return TickOutcome::Skipped {
                reason: SkipReason::NoTemplate,
            };
        }

        // ── Steps 8 + 9: generate + lint (with one regenerate) ───────────────
        let situation_str = format!("{:?}", situation);
        let locale = self.locale.clone();

        let body = match self
            .generate_with_lint(&id, &situation_str, &locale, now_utc)
            .await
        {
            GenerateResult::Ok(text) => text,
            GenerateResult::RateLimit => {
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.clone(),
                    reason: "llm_rate_limit_pending_m5_5".to_string(),
                });
                return TickOutcome::Skipped {
                    reason: SkipReason::LlmRateLimit,
                };
            }
            GenerateResult::LinterPersistent => {
                // MessageDropped already appended inside generate_with_lint.
                return TickOutcome::Skipped {
                    reason: SkipReason::LinterPersistent,
                };
            }
        };

        // ── Step 11: deliver ─────────────────────────────────────────────────
        let msg = CompanionMessage {
            id: id.clone(),
            situation: situation.clone(),
            template_id: template_id.clone(),
            locale: locale.clone(),
            body,
            generated_at: now_utc,
        };

        match self.notifier.send(&msg).await {
            Ok(NotifyOutcome::Delivered) => {
                // continue to step 12
            }
            Ok(NotifyOutcome::Skipped { reason }) => {
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.clone(),
                    reason: format!("notifier_skipped:{reason}"),
                });
                return TickOutcome::Skipped {
                    reason: SkipReason::NotifierSkipped,
                };
            }
            Ok(NotifyOutcome::Failed(_)) | Err(_) => {
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.clone(),
                    reason: "notifier_failed".to_string(),
                });
                return TickOutcome::Skipped {
                    reason: SkipReason::NotifierFailed,
                };
            }
        }

        // ── Step 12: finalise ────────────────────────────────────────────────
        let _ = self.ledger.append(&OutboxEvent::MessageSent {
            id: id.clone(),
            channel: self.notifier.name().to_string(),
            sent_at: now_utc,
        });

        self.picker.record(&template_id, Signal::Sent, now_utc);
        self.last_send_at = Some(now_local);
        self.sent_today += 1;
        if situation == Situation::MorningGreeting {
            self.morning_sent_today = Some(today);
        }

        TickOutcome::Sent {
            id,
            situation,
            template_id,
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Internal helper: generate + lint with one regenerate
    // ─────────────────────────────────────────────────────────────────────────

    /// Attempt to generate a lint-passing body.
    ///
    /// On first lint failure, appends `MessageGenerated { regen_count: 0 }` and
    /// retries **once** with a `"\n[regenerate]"` suffix on the user prompt so
    /// the `StubLlm` can match a distinct scenario.  On second failure, appends
    /// both a second `MessageGenerated` and `MessageDropped { linter_persistent }`.
    async fn generate_with_lint(
        &mut self,
        id: &str,
        situation_str: &str,
        locale: &str,
        _now_utc: DateTime<Utc>,
    ) -> GenerateResult {
        for regen_count in 0u32..=1 {
            // Build the user prompt; append "[regenerate]" on the retry so
            // StubLlm can match a distinct scenario (documented contract).
            let user_prompt = if regen_count == 0 {
                format!(
                    "Compose one short message for situation: {situation_str}, locale: {locale}"
                )
            } else {
                format!(
                    "Compose one short message for situation: {situation_str}, locale: {locale}\n[regenerate]"
                )
            };

            let req = LlmRequest {
                messages: vec![
                    LlmMessage {
                        role: "system".to_string(),
                        content: self.voice_md.clone(),
                    },
                    LlmMessage {
                        role: "user".to_string(),
                        content: user_prompt,
                    },
                ],
                temperature: None,
                max_tokens: None,
            };

            let text = match self.llm.generate(req).await {
                Ok(resp) => resp.text,
                Err(LlmError::RateLimit) => return GenerateResult::RateLimit,
                Err(e) => {
                    tracing::warn!("outbox: LLM error on attempt {regen_count}: {e}");
                    // Treat other errors like a lint failure — drop after second.
                    if regen_count == 1 {
                        let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                            id: id.to_string(),
                            reason: "linter_persistent".to_string(),
                        });
                        return GenerateResult::LinterPersistent;
                    }
                    continue;
                }
            };

            let report = linter::check(&text, locale);
            let body_sha256 = hex::encode(Sha256::digest(text.as_bytes()));

            let _ = self.ledger.append(&OutboxEvent::MessageGenerated {
                id: id.to_string(),
                locale_used: locale.to_string(),
                body_sha256,
                linter_violations: report.violations.len() as u32,
                regen_count,
            });

            if report.passed {
                return GenerateResult::Ok(text);
            }

            // Lint failed.
            if regen_count == 1 {
                // Second failure — drop.
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.to_string(),
                    reason: "linter_persistent".to_string(),
                });
                return GenerateResult::LinterPersistent;
            }
            // regen_count == 0 → loop continues with regen_count = 1
        }

        // Unreachable, but satisfies the compiler.
        GenerateResult::LinterPersistent
    }
}

/// Internal result of the generate+lint loop.
enum GenerateResult {
    Ok(String),
    RateLimit,
    LinterPersistent,
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use crate::companion::clock::MockClock;
    use crate::companion::notifier::{CompanionMessage, NotifyOutcome};
    use crate::companion::picker::{BanditState, Picker, TemplateState};
    use crate::companion::telemetry::OutboxEvent;
    use crate::durable::ledger::Ledger;
    use crate::llm::stub::StubLlm;
    use crate::llm::LlmClient;
    use anyhow::Result as AnyhowResult;
    use async_trait::async_trait;
    use chrono::{Duration, Local, NaiveDate, NaiveTime, TimeZone};
    use mur_common::agent::{ProactiveConfig, QuietHours};
    use mur_common::companion::Situation;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    // ── FakeNotifier ─────────────────────────────────────────────────────────

    /// In-test notifier that records every delivered message and can be
    /// configured to return a fixed outcome.
    struct FakeNotifier {
        outcome: NotifyOutcomeKind,
        calls: Mutex<Vec<CompanionMessage>>,
    }

    enum NotifyOutcomeKind {
        Delivered,
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
                NotifyOutcomeKind::Skipped(r) => NotifyOutcome::Skipped {
                    reason: r.clone(),
                },
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
            },
        )
    }

    // ─────────────────────────────────────────────────────────────────────────
    // M5.3 tests (must remain green after M5.4 changes)
    // ─────────────────────────────────────────────────────────────────────────

    // Test 1: proactive disabled → 24 h sim yields 0 scheduled
    #[tokio::test]
    async fn proactive_disabled_runs_24h_zero_scheduled() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 8, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 42);
        let proactive = make_proactive(false, 3, None, None);
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            Arc::new(FakeNotifier::delivered()),
        );

        let ticks = 24 * 60;
        for _ in 0..ticks {
            let now_utc = clock.now_utc();
            let now_local = clock.now_local();
            let outcome = outbox.run_tick(now_utc, now_local).await;
            assert!(
                matches!(
                    outcome,
                    TickOutcome::Skipped {
                        reason: SkipReason::GateBlocked(BlockReason::ProactiveDisabled)
                    }
                ),
                "expected ProactiveDisabled, got {outcome:?}"
            );
            clock.advance(Duration::seconds(60));
        }

        assert_eq!(
            count_scheduled(tmp.path()),
            0,
            "ledger must have zero MessageScheduled events when proactive is disabled"
        );
    }

    // Test 2: enabled, cap=3, 12 h window → exactly 3 MessageScheduled + 3 MessageSent
    #[tokio::test]
    async fn enabled_cap3_window12h_yields_exactly_3_scheduled() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 3, Some("22:00"), Some("23:59"));
        let notifier = Arc::new(FakeNotifier::delivered());
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            notifier.clone(),
        );

        let ticks = 12 * 60;
        let mut sent_count = 0usize;
        for _ in 0..ticks {
            let now_utc = clock.now_utc();
            let now_local = clock.now_local();
            let outcome = outbox.run_tick(now_utc, now_local).await;
            if matches!(outcome, TickOutcome::Sent { .. }) {
                sent_count += 1;
            }
            clock.advance(Duration::seconds(60));
        }

        assert_eq!(
            sent_count, 3,
            "expected exactly 3 Sent outcomes across a 12h window with cap=3"
        );
        assert_eq!(
            count_scheduled(tmp.path()),
            3,
            "ledger must have exactly 3 MessageScheduled events"
        );
    }

    // Test 3: morning_greeting only fires once per day
    #[tokio::test]
    async fn morning_greeting_only_once_per_day() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 6, 30, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 0);
        let proactive = make_proactive(true, 10, None, None);
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            Arc::new(FakeNotifier::delivered()),
        );

        let mut morning_count = 0usize;
        for _ in 0..48 {
            let now_utc = clock.now_utc();
            let now_local = clock.now_local();
            if let TickOutcome::Sent { situation, .. } = outbox.run_tick(now_utc, now_local).await {
                if situation == Situation::MorningGreeting {
                    morning_count += 1;
                }
            }
            clock.advance(Duration::minutes(5));
        }

        assert_eq!(
            morning_count, 1,
            "MorningGreeting should fire at most once per day; got {morning_count}"
        );
    }

    // Test 4: day rollover resets counters
    #[tokio::test]
    async fn day_rollover_resets_counters() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 7);
        // cap=1 → only 1 send per day
        let proactive = make_proactive(true, 1, Some("22:00"), Some("23:59"));
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            Arc::new(FakeNotifier::delivered()),
        );

        // Day N: first tick should send
        let outcome_day_n = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        assert!(
            matches!(outcome_day_n, TickOutcome::Sent { .. }),
            "day N first tick should send; got {outcome_day_n:?}"
        );
        assert_eq!(outbox.sent_today, 1);

        // Advance 25 h → day N+1
        clock.advance(Duration::hours(25));

        let outcome_day_n1 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        assert_eq!(
            outbox.sent_today, 1,
            "after rollover sent_today should be 1 (just incremented from 0)"
        );
        assert!(
            matches!(outcome_day_n1, TickOutcome::Sent { .. }),
            "day N+1 first tick should send after rollover; got {outcome_day_n1:?}"
        );

        assert_eq!(
            count_scheduled(tmp.path()),
            2,
            "ledger should have 2 MessageScheduled events (one per day)"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // M5.4 new tests
    // ─────────────────────────────────────────────────────────────────────────

    // M5.4 Test 1: clean body → MessageScheduled → MessageGenerated(regen=0) → MessageSent
    #[tokio::test]
    async fn linter_pass_emits_generated_then_sent() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 5, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            notifier.clone(),
        );

        let now_utc = clock.now_utc();
        let now_local = clock.now_local();
        let outcome = outbox.run_tick(now_utc, now_local).await;

        assert!(
            matches!(outcome, TickOutcome::Sent { .. }),
            "expected Sent, got {outcome:?}"
        );
        assert_eq!(notifier.call_count(), 1, "notifier should have been called once");

        let events = all_events(tmp.path());
        let has_scheduled = events.iter().any(|e| matches!(e, OutboxEvent::MessageScheduled { .. }));
        let has_generated = events.iter().any(|e| matches!(
            e,
            OutboxEvent::MessageGenerated { regen_count: 0, .. }
        ));
        let has_sent = events.iter().any(|e| matches!(e, OutboxEvent::MessageSent { .. }));

        assert!(has_scheduled, "must have MessageScheduled; events: {events:?}");
        assert!(has_generated, "must have MessageGenerated(regen_count=0); events: {events:?}");
        assert!(has_sent, "must have MessageSent; events: {events:?}");
    }

    // M5.4 Test 2: first generate → banned body → regenerate → clean body → MessageSent
    #[tokio::test]
    async fn linter_first_fail_regenerates_then_passes() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 5, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_bad_then_clean_zh(),
            notifier.clone(),
        );

        let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        assert!(
            matches!(outcome, TickOutcome::Sent { .. }),
            "expected Sent after regenerate, got {outcome:?}"
        );
        assert_eq!(notifier.call_count(), 1);

        let events = all_events(tmp.path());

        // Must have TWO MessageGenerated events: regen_count 0 then 1.
        let generated_events: Vec<&OutboxEvent> = events
            .iter()
            .filter(|e| matches!(e, OutboxEvent::MessageGenerated { .. }))
            .collect();
        assert_eq!(
            generated_events.len(),
            2,
            "expected 2 MessageGenerated events; got: {generated_events:?}"
        );
        assert!(
            matches!(generated_events[0], OutboxEvent::MessageGenerated { regen_count: 0, .. }),
            "first MessageGenerated must have regen_count=0"
        );
        assert!(
            matches!(generated_events[1], OutboxEvent::MessageGenerated { regen_count: 1, .. }),
            "second MessageGenerated must have regen_count=1"
        );

        // Final event is MessageSent.
        assert!(
            events.iter().any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
            "must end with MessageSent"
        );
    }

    // M5.4 Test 3: banned body twice → MessageDropped(linter_persistent)
    #[tokio::test]
    async fn linter_persistent_drops() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 5, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_always_banned_zh(),
            notifier.clone(),
        );

        let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        assert!(
            matches!(
                outcome,
                TickOutcome::Skipped {
                    reason: SkipReason::LinterPersistent
                }
            ),
            "expected LinterPersistent, got {outcome:?}"
        );
        assert_eq!(notifier.call_count(), 0, "notifier must not be called");

        let events = all_events(tmp.path());

        let generated_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, OutboxEvent::MessageGenerated { .. }))
            .collect();
        assert_eq!(
            generated_events.len(),
            2,
            "expected 2 MessageGenerated events for both attempts"
        );

        assert!(
            events.iter().any(|e| matches!(
                e,
                OutboxEvent::MessageDropped { reason, .. } if reason == "linter_persistent"
            )),
            "must have MessageDropped(linter_persistent)"
        );
        assert!(
            !events.iter().any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
            "must NOT have MessageSent"
        );
    }

    // M5.4 Test 4: notifier fails → MessageDropped(notifier_failed)
    #[tokio::test]
    async fn notifier_failed_drops() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 5, None, None);
        let notifier = Arc::new(FakeNotifier::failed());
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            notifier.clone(),
        );

        let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        assert!(
            matches!(
                outcome,
                TickOutcome::Skipped {
                    reason: SkipReason::NotifierFailed
                }
            ),
            "expected NotifierFailed, got {outcome:?}"
        );

        let events = all_events(tmp.path());
        assert!(
            events.iter().any(|e| matches!(
                e,
                OutboxEvent::MessageDropped { reason, .. } if reason == "notifier_failed"
            )),
            "must have MessageDropped(notifier_failed)"
        );
        assert!(
            !events.iter().any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
            "must NOT have MessageSent"
        );
    }
}
