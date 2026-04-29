//! Outbox tick loop — Spec §4.8 steps 1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12.
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
//! - `pending_pause`: in-memory map of paused sends (id → PendingPause).
//!
//! ## Locale resolution (step 10)
//!
//! The locale is taken from `proactive.locale` if that field exists — but
//! `ProactiveConfig` does not carry a `locale` field in the current schema.
//! For M5.4 we therefore fall back to the `locale` field carried in the
//! `Outbox` struct itself, which callers set from `CompanionConfig.locale`
//! (the field closest to the spec's "agent_profile.locale").
//!
//! ## Phase 1.1 M5.5 pause-state limitation
//!
//! Phase 1.1 M5.5 holds pause state in-memory only.  After supervisor restart,
//! in-flight paused sends are lost; the next ledger scan only resumes events
//! but does not re-attempt from scratch.  Persistence of `pending_pause` to
//! disk is deferred to a follow-up task.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDate, Utc};
use mur_common::companion::{Signal, Situation};
use rand::{RngCore, rngs::StdRng};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::companion::{
    clock::Clock,
    earned_permission::{self, BlockReason, GateOutcome},
    i18n::{self, EnsureLocaleOutcome},
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
// Retry backoff schedule (Spec §6.2)
// ──────────────────────────────────────────────────────────────────────────────

/// Deterministic retry backoff delays: 30 s, 90 s, 4 min, 15 min.
/// Index corresponds to the attempt number (0-based).
/// `backoff_for_attempt(4)` returns `None` → terminal drop.
const RETRY_BACKOFF_SECS: [i64; 4] = [30, 90, 240, 900];

fn backoff_for_attempt(attempt: u8) -> Option<chrono::Duration> {
    RETRY_BACKOFF_SECS
        .get(attempt as usize)
        .map(|s| chrono::Duration::seconds(*s))
}

// ──────────────────────────────────────────────────────────────────────────────
// PendingPause — in-memory pause state
// ──────────────────────────────────────────────────────────────────────────────

/// Why a send was paused.
#[derive(Debug, Clone)]
pub enum PauseKind {
    /// Translation LLM returned rate-limit or an error.
    LocaleRetry,
    /// Generation LLM returned rate-limit.
    RateLimitGenerate,
}

/// In-memory pause record for a single scheduled message.
///
/// Populated when the outbox pauses a send; consumed when `resume_at` elapses
/// and the outbox re-attempts.
#[derive(Debug, Clone)]
pub struct PendingPause {
    pub situation: Situation,
    pub template_id: TemplateId,
    /// BCP-47 locale at scheduling time.
    pub locale_at_schedule: String,
    /// Number of pause attempts so far (starts at 0; incremented before each retry).
    pub attempts: u8,
    pub kind: PauseKind,
    pub resume_at: DateTime<Utc>,
    /// The already-generated (and linted) body.  `None` when the generation itself
    /// failed (RateLimitGenerate); `Some(body)` when only translation failed.
    pub body: Option<String>,
}

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
    /// LLM generation returned a rate-limit error; send is paused until `resume_at`.
    /// (Spec §6.3)
    PausedRateLimit { resume_at: DateTime<Utc> },
    /// Linter failed on both the original and regenerated body.
    LinterPersistent,
    /// Notifier returned `Failed`.
    NotifierFailed,
    /// Notifier returned `Skipped`.
    NotifierSkipped,
    /// Translation retries exhausted after 4 failures.
    /// (Spec §6.2)
    LocaleUnresolved,
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

    // ── pause state (in-memory only; Phase 1.1 M5.5 limitation) ──
    /// In-memory map of paused sends.  Key = message id.
    ///
    /// Phase 1.1 M5.5: holds pause state in memory only.  After supervisor
    /// restart, in-flight paused sends are lost.  Disk persistence is deferred.
    pub pending_pause: BTreeMap<String, PendingPause>,
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
            pending_pause: BTreeMap::new(),
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
            pending_pause: BTreeMap::new(),
        }
    }

    /// Execute one tick of the outbox loop.
    ///
    /// Steps implemented: **1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12**.
    /// Step 3 (passive-dismiss) → M5.6.
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

        // ── Step 2: resume paused sends ──────────────────────────────────────
        // Check in-memory pending_pause map for entries whose resume_at has
        // elapsed.  Attempt re-delivery for each one.  If a resumed send
        // succeeds, return `TickOutcome::Sent` immediately (the tick is done).
        // If it fails again, re-park it.
        //
        // We collect the IDs first to avoid borrow conflicts.
        let resumable_ids: Vec<String> = self
            .pending_pause
            .iter()
            .filter(|(_, p)| p.resume_at <= now_utc)
            .map(|(id, _)| id.clone())
            .collect();

        for id in resumable_ids {
            // Move the PendingPause out of the map; re-insert on re-pause.
            let Some(mut paused) = self.pending_pause.remove(&id) else {
                continue;
            };

            // Increment attempts before deciding.
            paused.attempts += 1;

            match paused.kind {
                PauseKind::RateLimitGenerate => {
                    // Re-attempt generation.
                    let situation_str = format!("{:?}", paused.situation);
                    let locale = paused.locale_at_schedule.clone();
                    match self
                        .generate_with_lint(&id, &situation_str, &locale, now_utc)
                        .await
                    {
                        GenerateResult::Ok(body) => {
                            // Step 10: i18n on resumed body.
                            let deliver_body = match self
                                .handle_i18n(&id, &body, &locale, now_utc, paused.attempts)
                                .await
                            {
                                I18nResult::UseBody(b) => b,
                                I18nResult::Paused(resume_at) => {
                                    // Re-park as LocaleRetry.
                                    self.pending_pause.insert(
                                        id.clone(),
                                        PendingPause {
                                            kind: PauseKind::LocaleRetry,
                                            body: Some(body),
                                            attempts: paused.attempts,
                                            resume_at,
                                            ..paused
                                        },
                                    );
                                    continue;
                                }
                                I18nResult::Terminal => {
                                    continue;
                                }
                            };
                            // Deliver — return early on success.
                            let outcome = self
                                .deliver_and_finalise(
                                    &id,
                                    &deliver_body,
                                    &locale,
                                    paused.situation.clone(),
                                    paused.template_id.clone(),
                                    now_utc,
                                    now_local,
                                )
                                .await;
                            if matches!(outcome, TickOutcome::Sent { .. }) {
                                return outcome;
                            }
                        }
                        GenerateResult::RateLimit => {
                            // Still rate-limited; schedule next backoff or drop.
                            if let Some(backoff) = backoff_for_attempt(paused.attempts) {
                                let resume_at = now_utc + backoff;
                                let _ = self.ledger.append(&OutboxEvent::MessagePaused {
                                    id: id.clone(),
                                    resume_at,
                                    reason: "rate_limit_429".to_string(),
                                });
                                self.pending_pause.insert(
                                    id.clone(),
                                    PendingPause {
                                        resume_at,
                                        attempts: paused.attempts,
                                        ..paused
                                    },
                                );
                            } else {
                                // Terminal drop.
                                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                                    id: id.clone(),
                                    reason: "rate_limit_terminal".to_string(),
                                });
                            }
                        }
                        GenerateResult::LinterPersistent => {
                            // Already dropped inside generate_with_lint.
                        }
                    }
                }

                PauseKind::LocaleRetry => {
                    // Body was already generated; only translation needs retry.
                    let body = paused.body.clone().unwrap_or_default();
                    let locale = paused.locale_at_schedule.clone();
                    match self
                        .handle_i18n(&id, &body, &locale, now_utc, paused.attempts)
                        .await
                    {
                        I18nResult::UseBody(deliver_body) => {
                            // Deliver — return early on success.
                            let outcome = self
                                .deliver_and_finalise(
                                    &id,
                                    &deliver_body,
                                    &locale,
                                    paused.situation.clone(),
                                    paused.template_id.clone(),
                                    now_utc,
                                    now_local,
                                )
                                .await;
                            if matches!(outcome, TickOutcome::Sent { .. }) {
                                return outcome;
                            }
                        }
                        I18nResult::Paused(resume_at) => {
                            self.pending_pause.insert(
                                id.clone(),
                                PendingPause {
                                    resume_at,
                                    attempts: paused.attempts,
                                    ..paused
                                },
                            );
                        }
                        I18nResult::Terminal => {
                            // Already logged inside handle_i18n.
                        }
                    }
                }
            }
        }

        // ── Step 3: passive-dismiss sweep ───────────────────────────────────
        // Scan up to 7 days of ledger to find MessageSent events that have
        // no UserSignal, PassiveDismissInferred, or MessageDropped.
        // For each such event older than 24 h, append PassiveDismissInferred
        // and record Signal::Dismiss on the picker.
        {
            let base_dir = self.ledger.base_dir().to_path_buf();
            let events: Vec<OutboxEvent> = Ledger::scan_days::<OutboxEvent>(&base_dir, 7)
                .into_iter()
                .filter_map(|r| r.ok())
                .collect();

            // id → template_id from MessageScheduled events
            let mut template_for_id: BTreeMap<String, TemplateId> = BTreeMap::new();
            // id → sent_at from MessageSent events
            let mut sent_at_for_id: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
            // ids that already have a UserSignal, PassiveDismissInferred, or MessageDropped
            let mut acked_ids: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            for event in &events {
                match event {
                    OutboxEvent::MessageScheduled { id, template_id, .. } => {
                        template_for_id.insert(id.clone(), template_id.clone());
                    }
                    OutboxEvent::MessageSent { id, sent_at, .. } => {
                        sent_at_for_id.insert(id.clone(), *sent_at);
                    }
                    OutboxEvent::UserSignal { id, .. } => {
                        acked_ids.insert(id.clone());
                    }
                    OutboxEvent::PassiveDismissInferred { id, .. } => {
                        acked_ids.insert(id.clone());
                    }
                    OutboxEvent::MessageDropped { id, .. } => {
                        acked_ids.insert(id.clone());
                    }
                    _ => {}
                }
            }

            for (id, sent_at) in &sent_at_for_id {
                if acked_ids.contains(id.as_str()) {
                    continue;
                }
                if (now_utc - *sent_at) <= chrono::Duration::hours(24) {
                    continue;
                }
                // Needs passive dismiss.
                let _ = self.ledger.append(&OutboxEvent::PassiveDismissInferred {
                    id: id.clone(),
                    at: now_utc,
                });
                if let Some(template_id) = template_for_id.get(id) {
                    self.picker.record(template_id, Signal::Dismiss, now_utc);
                }
            }
        }

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
                // TODO(M5.x or later): wire raw HeaderMap from anthropic.rs once that
                // surfaces 429 details; for now use deterministic backoff schedule.
                //
                // Attempt index 0 → first pause; if later attempts exhaust backoffs,
                // they are handled in the resume loop.
                let attempt: u8 = 0;
                if let Some(backoff) = backoff_for_attempt(attempt) {
                    let resume_at = now_utc + backoff;
                    let _ = self.ledger.append(&OutboxEvent::MessagePaused {
                        id: id.clone(),
                        resume_at,
                        reason: "rate_limit_429".to_string(),
                    });
                    self.pending_pause.insert(
                        id.clone(),
                        PendingPause {
                            situation,
                            template_id,
                            locale_at_schedule: locale,
                            attempts: attempt,
                            kind: PauseKind::RateLimitGenerate,
                            resume_at,
                            body: None,
                        },
                    );
                    return TickOutcome::Skipped {
                        reason: SkipReason::PausedRateLimit { resume_at },
                    };
                }
                // Should not happen at attempt 0, but handle defensively.
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.clone(),
                    reason: "rate_limit_terminal".to_string(),
                });
                return TickOutcome::Skipped {
                    reason: SkipReason::PausedRateLimit {
                        resume_at: now_utc,
                    },
                };
            }
            GenerateResult::LinterPersistent => {
                // MessageDropped already appended inside generate_with_lint.
                return TickOutcome::Skipped {
                    reason: SkipReason::LinterPersistent,
                };
            }
        };

        // ── Step 10: i18n ensure_locale ──────────────────────────────────────
        let deliver_body = match self
            .handle_i18n(&id, &body, &locale, now_utc, 0)
            .await
        {
            I18nResult::UseBody(b) => b,
            I18nResult::Paused(resume_at) => {
                self.pending_pause.insert(
                    id.clone(),
                    PendingPause {
                        situation,
                        template_id,
                        locale_at_schedule: locale,
                        attempts: 0,
                        kind: PauseKind::LocaleRetry,
                        resume_at,
                        body: Some(body),
                    },
                );
                return TickOutcome::Skipped {
                    reason: SkipReason::LocaleUnresolved,
                };
            }
            I18nResult::Terminal => {
                return TickOutcome::Skipped {
                    reason: SkipReason::LocaleUnresolved,
                };
            }
        };

        // ── Step 11: deliver ─────────────────────────────────────────────────
        self.deliver_and_finalise(
            &id,
            &deliver_body,
            &locale,
            situation.clone(),
            template_id.clone(),
            now_utc,
            now_local,
        )
        .await
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Internal helper: i18n step
    // ─────────────────────────────────────────────────────────────────────────

    /// Call `i18n::ensure_locale` and interpret the result.
    ///
    /// `attempt_index` is the current attempt (0 = first try from the new-send
    /// path; ≥1 means we are in a resume cycle).
    ///
    /// On `QueuedRetry`: if `attempt_index` < 4 → schedule next backoff and
    /// return `I18nResult::Paused`; if attempt_index ≥ 4 → terminal drop.
    async fn handle_i18n(
        &mut self,
        id: &str,
        body: &str,
        locale: &str,
        now_utc: DateTime<Utc>,
        attempt_index: u8,
    ) -> I18nResult {
        match i18n::ensure_locale(body, locale, self.llm.as_ref(), false).await {
            EnsureLocaleOutcome::Original => I18nResult::UseBody(body.to_string()),
            EnsureLocaleOutcome::Translated(new_body) => I18nResult::UseBody(new_body),
            // Unreachable in proactive path (reactive=false), but treat as Original.
            EnsureLocaleOutcome::OriginalWithLog(_err) => I18nResult::UseBody(body.to_string()),
            EnsureLocaleOutcome::QueuedRetry(_err) => {
                // append LocaleMismatchUnresolved event for this attempt.
                let _ = self.ledger.append(&OutboxEvent::LocaleMismatchUnresolved {
                    id: id.to_string(),
                    attempts: attempt_index + 1,
                    at: now_utc,
                });

                if let Some(backoff) = backoff_for_attempt(attempt_index) {
                    let resume_at = now_utc + backoff;
                    let _ = self.ledger.append(&OutboxEvent::MessagePaused {
                        id: id.to_string(),
                        resume_at,
                        reason: "locale_retry".to_string(),
                    });
                    I18nResult::Paused(resume_at)
                } else {
                    // 4th failure — terminal drop (attempts 0..3 exhausted).
                    let _ = self.ledger.append(&OutboxEvent::LocaleMismatchUnresolved {
                        id: id.to_string(),
                        attempts: 4,
                        at: now_utc,
                    });
                    let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                        id: id.to_string(),
                        reason: "locale_unresolved".to_string(),
                    });
                    I18nResult::Terminal
                }
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Internal helper: deliver + finalise (step 11 + 12)
    // ─────────────────────────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    async fn deliver_and_finalise(
        &mut self,
        id: &str,
        body: &str,
        locale: &str,
        situation: Situation,
        template_id: TemplateId,
        now_utc: DateTime<Utc>,
        now_local: DateTime<Local>,
    ) -> TickOutcome {
        let msg = CompanionMessage {
            id: id.to_string(),
            situation: situation.clone(),
            template_id: template_id.clone(),
            locale: locale.to_string(),
            body: body.to_string(),
            generated_at: now_utc,
        };

        match self.notifier.send(&msg).await {
            Ok(NotifyOutcome::Delivered) => {
                // continue to step 12
            }
            Ok(NotifyOutcome::Skipped { reason }) => {
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.to_string(),
                    reason: format!("notifier_skipped:{reason}"),
                });
                return TickOutcome::Skipped {
                    reason: SkipReason::NotifierSkipped,
                };
            }
            Ok(NotifyOutcome::Failed(_)) | Err(_) => {
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.to_string(),
                    reason: "notifier_failed".to_string(),
                });
                return TickOutcome::Skipped {
                    reason: SkipReason::NotifierFailed,
                };
            }
        }

        // ── Step 12: finalise ────────────────────────────────────────────────
        let _ = self.ledger.append(&OutboxEvent::MessageSent {
            id: id.to_string(),
            channel: self.notifier.name().to_string(),
            sent_at: now_utc,
        });

        let today = now_local.date_naive();
        self.picker.record(&template_id, Signal::Sent, now_utc);
        self.last_send_at = Some(now_local);
        self.sent_today += 1;
        if situation == Situation::MorningGreeting {
            self.morning_sent_today = Some(today);
        }

        TickOutcome::Sent {
            id: id.to_string(),
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

/// Internal result of the i18n step.
enum I18nResult {
    /// Caller should use this body (original or translated).
    UseBody(String),
    /// Translation failed; send was paused until `resume_at`.
    Paused(DateTime<Utc>),
    /// Terminal: 4 failures exhausted; message dropped.
    Terminal,
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
    // M5.3 tests (must remain green after M5.4/M5.5 changes)
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

    // ─────────────────────────────────────────────────────────────────────────
    // M5.5 new tests
    // ─────────────────────────────────────────────────────────────────────────

    // M5.5 Test 1: mixed-language body (passes linter, fails heuristic_matches),
    //              locale = zh-TW → stub returns translated zh-TW body → notifier
    //              sees translated body.
    //
    // We use "Ok, 好。" as the generated body because:
    //   - linter: "Ok," has comma so is NOT an all-ascii-alpha token; 0/2 English
    //     tokens = 0% → passes PreservedEnglishRatioZh.  1 sentence → passes.
    //   - heuristic_matches: CJK char "好" = 1 out of 5 non-whitespace = 20% < 30%
    //     → fails → translation is triggered.
    #[tokio::test]
    async fn i18n_force_english_in_zh_locale_translates_then_sends() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 5, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());

        // Stub: generate returns a mixed body that passes the linter but triggers
        // translation (CJK ratio too low for zh-TW).  Translate call returns
        // the full zh-TW body.
        let translated = "早安！今天想從哪一件小事開始？";
        let llm = Arc::new(
            StubLlm::from_yaml(&format!(
                r#"
- match:
    contains: "Translate the following"
  response: "{translated}"
- match:
    contains: "situation:"
  response: "Ok, 好。"
"#
            ))
            .unwrap(),
        );

        let mut outbox = Outbox::with_picker(
            clock.clone(),
            ledger,
            picker,
            proactive,
            OutboxConfig {
                llm,
                notifier: notifier.clone(),
                voice_md: "You are a warm companion.".to_string(),
                locale: "zh-TW".to_string(),
            },
        );

        let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        assert!(
            matches!(outcome, TickOutcome::Sent { .. }),
            "expected Sent, got {outcome:?}"
        );
        assert_eq!(notifier.call_count(), 1);
        assert_eq!(
            notifier.last_body().as_deref(),
            Some(translated),
            "notifier should have received translated body"
        );

        let events = all_events(tmp.path());
        assert!(events.iter().any(|e| matches!(e, OutboxEvent::MessageScheduled { .. })));
        // MessageGenerated should have the English body sha256 (pre-translation).
        assert!(events.iter().any(|e| matches!(e, OutboxEvent::MessageGenerated { .. })));
        assert!(events.iter().any(|e| matches!(e, OutboxEvent::MessageSent { .. })));
    }

    // M5.5 Test 2: generate succeeds (mixed body that triggers translation),
    //              translate → RateLimit on tick 1; tick 2 (after resume_at):
    //              translate succeeds → MessageSent.
    #[tokio::test]
    async fn translate_429_pauses_then_resumes_after_resume_at() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 5, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());

        let translated = "早安！今天想從哪一件小事開始？";

        // Use a counter to flip behavior: first translate attempt → RateLimit,
        // subsequent → success.  We use a shared Mutex<u32> inside a custom LLM.
        use crate::llm::{LlmError, LlmRequest, LlmResponse};

        struct FlipLlm {
            translate_calls: Mutex<u32>,
            translated: String,
        }

        #[async_trait]
        impl LlmClient for FlipLlm {
            async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
                let joined: String = req.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n");
                if joined.contains("Translate the following") {
                    let mut calls = self.translate_calls.lock().unwrap();
                    *calls += 1;
                    if *calls == 1 {
                        return Err(LlmError::RateLimit);
                    }
                    return Ok(LlmResponse {
                        text: self.translated.clone(),
                        input_tokens: 0,
                        output_tokens: 0,
                        model: "flip".into(),
                    });
                }
                // Generation call: return a body that passes linter but triggers
                // translation (CJK ratio < 30% for zh-TW).
                Ok(LlmResponse {
                    text: "Ok, 好。".into(),
                    input_tokens: 0,
                    output_tokens: 0,
                    model: "flip".into(),
                })
            }
            fn model_name(&self) -> &str { "flip" }
        }

        let llm: Arc<dyn LlmClient> = Arc::new(FlipLlm {
            translate_calls: Mutex::new(0),
            translated: translated.to_string(),
        });

        let mut outbox = Outbox::with_picker(
            clock.clone(),
            ledger,
            picker,
            proactive,
            OutboxConfig {
                llm,
                notifier: notifier.clone(),
                voice_md: "You are a warm companion.".to_string(),
                locale: "zh-TW".to_string(),
            },
        );

        // Tick 1: translate fails → paused.
        let outcome1 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        assert!(
            matches!(
                outcome1,
                TickOutcome::Skipped {
                    reason: SkipReason::LocaleUnresolved
                }
            ),
            "tick 1 should pause on translate failure; got {outcome1:?}"
        );
        assert_eq!(notifier.call_count(), 0, "notifier must not be called on tick 1");

        // Ledger must have MessagePaused but no MessageSent.
        {
            let events = all_events(tmp.path());
            assert!(
                events.iter().any(|e| matches!(e, OutboxEvent::MessagePaused { .. })),
                "must have MessagePaused after tick 1"
            );
            assert!(
                !events.iter().any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
                "must NOT have MessageSent after tick 1"
            );
        }

        // Advance clock past the first backoff (30 s).
        clock.advance(Duration::seconds(31));

        // Tick 2: resume_at has elapsed; translate succeeds → sent.
        let outcome2 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        assert!(
            matches!(outcome2, TickOutcome::Sent { .. }),
            "tick 2 should succeed; got {outcome2:?}"
        );
        assert_eq!(notifier.call_count(), 1);
        assert_eq!(
            notifier.last_body().as_deref(),
            Some(translated),
            "notifier should have received translated body"
        );
    }

    // M5.5 Test 3: generate succeeds; translate always → RateLimit; 4 ticks →
    //              MessageDropped(locale_unresolved) + LocaleMismatchUnresolved{attempts:4}.
    #[tokio::test]
    async fn translate_4_failures_drops_locale_unresolved() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 10, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());

        use crate::llm::{LlmError, LlmRequest, LlmResponse};

        struct AlwaysRateLimitTranslate;

        #[async_trait]
        impl LlmClient for AlwaysRateLimitTranslate {
            async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
                let joined: String = req.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n");
                if joined.contains("Translate the following") {
                    return Err(LlmError::RateLimit);
                }
                // Generation returns a body that passes the linter but triggers
                // translation (CJK ratio < 30% for zh-TW).
                Ok(LlmResponse {
                    text: "Ok, 好。".into(),
                    input_tokens: 0,
                    output_tokens: 0,
                    model: "always-rl".into(),
                })
            }
            fn model_name(&self) -> &str { "always-rl" }
        }

        let llm: Arc<dyn LlmClient> = Arc::new(AlwaysRateLimitTranslate);

        let mut outbox = Outbox::with_picker(
            clock.clone(),
            ledger,
            picker,
            proactive,
            OutboxConfig {
                llm,
                notifier: notifier.clone(),
                voice_md: "You are a warm companion.".to_string(),
                locale: "zh-TW".to_string(),
            },
        );

        // Backoff schedule: [30s, 90s, 240s, 900s].
        // Tick 1: new send → translate fails → pause(attempt=0, backoff=30s).
        let outcome1 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        assert!(
            matches!(outcome1, TickOutcome::Skipped { reason: SkipReason::LocaleUnresolved }),
            "tick 1: {outcome1:?}"
        );

        // Advance past 30s → tick 2: resume → translate fails again → pause(attempt=1, backoff=90s).
        clock.advance(Duration::seconds(35));
        let outcome2 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        // Tick 2 may Sent a new message or Skipped — what matters is the resumed
        // attempt fails again.  We check ledger at the end.
        let _ = outcome2;

        // Advance past 90s → tick 3: resume → translate fails → pause(attempt=2, backoff=240s).
        clock.advance(Duration::seconds(95));
        let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

        // Advance past 240s → tick 4: resume → translate fails → attempt=3 → backoff=900s.
        clock.advance(Duration::seconds(245));
        let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

        // Advance past 900s → tick 5: resume → translate fails → attempt=4 → TERMINAL drop.
        clock.advance(Duration::seconds(905));
        let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

        // Check ledger.
        let events = all_events(tmp.path());
        assert!(
            events.iter().any(|e| matches!(
                e,
                OutboxEvent::MessageDropped { reason, .. } if reason == "locale_unresolved"
            )),
            "must have MessageDropped(locale_unresolved); events: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                OutboxEvent::LocaleMismatchUnresolved { attempts: 4, .. }
            )),
            "must have LocaleMismatchUnresolved{{attempts: 4}}; events: {events:?}"
        );
        assert_eq!(notifier.call_count(), 0, "notifier must never be called");
    }

    // M5.5 Test 4: generate stub → RateLimit → ledger has MessagePaused{reason:rate_limit_429};
    //              outcome = Skipped{PausedRateLimit}; sent_today unchanged.
    #[tokio::test]
    async fn llm_rate_limit_pauses_send() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 5, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());

        let llm = Arc::new(
            StubLlm::from_yaml(
                r#"
- match:
    contains: "situation:"
  fault: rate_limit
"#,
            )
            .unwrap(),
        );

        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            llm,
            notifier.clone(),
        );

        let now_utc = clock.now_utc();
        let outcome = outbox.run_tick(now_utc, clock.now_local()).await;

        // Verify outcome is PausedRateLimit.
        let resume_at = match &outcome {
            TickOutcome::Skipped {
                reason: SkipReason::PausedRateLimit { resume_at },
            } => *resume_at,
            other => panic!("expected PausedRateLimit, got {other:?}"),
        };
        assert!(
            resume_at > now_utc,
            "resume_at must be in the future; resume_at={resume_at}, now={now_utc}"
        );

        // sent_today must not have advanced.
        assert_eq!(outbox.sent_today, 0, "sent_today must be unchanged");

        // Ledger must have MessagePaused(rate_limit_429) but no MessageSent.
        let events = all_events(tmp.path());
        assert!(
            events.iter().any(|e| matches!(
                e,
                OutboxEvent::MessagePaused { reason, .. } if reason == "rate_limit_429"
            )),
            "must have MessagePaused(rate_limit_429); events: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
            "must NOT have MessageSent"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // M5.6 tests — passive-dismiss sweep
    // ─────────────────────────────────────────────────────────────────────────

    // Helper: drive one tick and return the sent message id + template_id.
    async fn drive_one_sent_tick(
        outbox: &mut Outbox<rand::rngs::StdRng>,
        clock: &Arc<MockClock>,
    ) -> (String, String) {
        let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
        match outcome {
            TickOutcome::Sent { id, template_id, .. } => (id, template_id),
            other => panic!("expected Sent, got {other:?}"),
        }
    }

    // M5.6 Test 1: passive_dismiss_fires_after_24h
    //
    // Send at T. Advance 24h+1min. Tick again. Ledger has PassiveDismissInferred
    // for the first id. Picker has dismiss_count > 0 for that template.
    #[tokio::test]
    async fn passive_dismiss_fires_after_24h() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 10, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            notifier.clone(),
        );

        // Tick 1: get a successful send.
        let (sent_id, template_id) = drive_one_sent_tick(&mut outbox, &clock).await;

        // Confirm no PassiveDismissInferred yet.
        {
            let events = all_events(tmp.path());
            assert!(
                !events.iter().any(|e| matches!(e, OutboxEvent::PassiveDismissInferred { .. })),
                "should not have PassiveDismissInferred before 24h"
            );
        }

        // Advance > 24h so the sweep fires.
        clock.advance(Duration::hours(24) + Duration::minutes(1));

        // Tick 2: sweep fires. May also schedule a new send — doesn't matter.
        let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

        // Ledger must contain a PassiveDismissInferred for `sent_id`.
        let events = all_events(tmp.path());
        let dismiss_ev = events.iter().find(|e| match e {
            OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
            _ => false,
        });
        assert!(
            dismiss_ev.is_some(),
            "PassiveDismissInferred must exist for id={sent_id}; events: {events:?}"
        );

        // Picker must have recorded a Dismiss for the template.
        let dismiss_count = outbox
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

    // M5.6 Test 2: passive_dismiss_skips_within_24h
    //
    // Send at T. Advance 23h 59min. Tick. No PassiveDismissInferred.
    #[tokio::test]
    async fn passive_dismiss_skips_within_24h() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 10, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            notifier.clone(),
        );

        // Tick 1: send at T.
        let (sent_id, _) = drive_one_sent_tick(&mut outbox, &clock).await;

        // Advance only 23h 59min — still within the 24h window.
        clock.advance(Duration::hours(23) + Duration::minutes(59));

        // Tick 2: sweep must NOT fire.
        let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

        let events = all_events(tmp.path());
        let dismiss_for_id = events.iter().any(|e| match e {
            OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
            _ => false,
        });
        assert!(
            !dismiss_for_id,
            "PassiveDismissInferred must NOT exist within 24h; events: {events:?}"
        );
    }

    // M5.6 Test 3: passive_dismiss_skips_when_acked
    //
    // Send at T. Manually append UserSignal{id, signal: Signal::Sent, at: T+1min}.
    // Advance 25h. Tick. No PassiveDismissInferred.
    #[tokio::test]
    async fn passive_dismiss_skips_when_acked() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 10, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            notifier.clone(),
        );

        // Tick 1: send at T.
        let (sent_id, _) = drive_one_sent_tick(&mut outbox, &clock).await;

        // Manually append a UserSignal to simulate that the user acknowledged.
        let ack_at = clock.now_utc() + Duration::minutes(1);
        let _ = outbox.ledger.append(&OutboxEvent::UserSignal {
            id: sent_id.clone(),
            signal: Signal::Positive,
            at: ack_at,
        });

        // Advance 25h.
        clock.advance(Duration::hours(25));

        // Tick 2: sweep must skip due to ack.
        let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

        let events = all_events(tmp.path());
        let dismiss_for_id = events.iter().any(|e| match e {
            OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
            _ => false,
        });
        assert!(
            !dismiss_for_id,
            "PassiveDismissInferred must NOT exist for acked message; events: {events:?}"
        );
    }

    // M5.6 Test 4: passive_dismiss_idempotent
    //
    // Send at T. Advance 25h. Tick (sweep fires once). Tick again (no time
    // advance). Ledger has exactly ONE PassiveDismissInferred for that id.
    #[tokio::test]
    async fn passive_dismiss_idempotent() {
        let tmp = TempDir::new().unwrap();
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        let proactive = make_proactive(true, 10, None, None);
        let notifier = Arc::new(FakeNotifier::delivered());
        let mut outbox = make_outbox(
            clock.clone(),
            ledger,
            picker,
            proactive,
            stub_clean_zh(),
            notifier.clone(),
        );

        // Tick 1: send at T.
        let (sent_id, _) = drive_one_sent_tick(&mut outbox, &clock).await;

        // Advance 25h so sweep is eligible.
        clock.advance(Duration::hours(25));

        // Tick 2: sweep fires — first PassiveDismissInferred written.
        let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

        // Count PassiveDismissInferred for sent_id after tick 2.
        let count_after_tick2 = all_events(tmp.path())
            .into_iter()
            .filter(|e| match e {
                OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
                _ => false,
            })
            .count();
        assert_eq!(
            count_after_tick2, 1,
            "expected exactly 1 PassiveDismissInferred after first sweep"
        );

        // Tick 3: same time — sweep must see existing PassiveDismissInferred in
        // acked_ids and not write a second one.
        let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

        let count_after_tick3 = all_events(tmp.path())
            .into_iter()
            .filter(|e| match e {
                OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
                _ => false,
            })
            .count();
        assert_eq!(
            count_after_tick3, 1,
            "PassiveDismissInferred must be idempotent — still exactly 1 after second sweep"
        );
    }
}
