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
use uuid::Uuid;

use crate::companion::{
    clock::Clock,
    earned_permission::{self, BlockReason, GateOutcome},
    notifier::Notifier,
    picker::{Picker, TemplateId},
    schedule::{self, ScheduleDecision},
    situations,
    telemetry::OutboxEvent,
};
use crate::durable::ledger::Ledger;
use crate::llm::LlmClient;
use mur_common::agent::ProactiveConfig;

mod deliver;
mod generate;
mod i18n;

#[cfg(test)]
mod tests;

use generate::GenerateResult;
use i18n::I18nResult;

// ──────────────────────────────────────────────────────────────────────────────
// Retry backoff schedule (Spec §6.2)
// ──────────────────────────────────────────────────────────────────────────────

/// Deterministic retry backoff delays: 30 s, 90 s, 4 min, 15 min.
/// Index corresponds to the attempt number (0-based).
/// `backoff_for_attempt(4)` returns `None` → terminal drop.
const RETRY_BACKOFF_SECS: [i64; 4] = [30, 90, 240, 900];

pub(super) fn backoff_for_attempt(attempt: u8) -> Option<chrono::Duration> {
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
    /// Map of `template_id → prompt_seed` loaded from the agent's content
    /// pool (`<agent_dir>/companion/content/<situation>.<locale>.yaml`,
    /// with embedded fallback). Populated at startup by `Companion::new`.
    ///
    /// M2.2.4: when the picker selects a `template_id` whose entry exists
    /// here with a non-empty seed, the outbox uses that seed (after
    /// placeholder substitution) as the LLM user prompt; otherwise it
    /// falls back to the legacy `"Compose one short message…"` line.
    pub prompt_seeds: BTreeMap<TemplateId, String>,
    /// `name_for_user` — used to substitute `{{NAME_FOR_USER}}` in `prompt_seed`.
    /// Comes from `profile.companion.voice_overrides.name_for_user`.
    pub name_for_user: String,
    /// First-memory text loaded from `relationship.json` — used to substitute
    /// `{{FIRST_MEMORY}}` / `{{FIRST_MEMORY_PARAGRAPH}}` in `prompt_seed`.
    /// `None` (or empty) collapses both placeholders to the empty string.
    pub first_memory: Option<String>,
    /// `formality` (lowercased Debug rendering, e.g. `"casual"`) — used to
    /// substitute `{{FORMALITY}}`.
    pub formality: String,
    /// `extra_instructions` — used to substitute `{{EXTRA_INSTRUCTIONS}}`.
    pub extra_instructions: String,
    /// Companion `relationship` — used to construct the `VoiceInput` for
    /// substitution. Not directly templated, but `voice::apply_placeholders`
    /// requires it.
    pub relationship: mur_common::companion::Relationship,
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

    // ── prompt_seed substitution context (M2.2.4) ──
    /// Map of `template_id → prompt_seed`. See [`OutboxConfig::prompt_seeds`].
    pub prompt_seeds: BTreeMap<TemplateId, String>,
    /// Owned placeholder context — see the corresponding `OutboxConfig` fields.
    pub name_for_user: String,
    pub first_memory: Option<String>,
    pub formality: String,
    pub extra_instructions: String,
    pub relationship: mur_common::companion::Relationship,

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
            prompt_seeds: config.prompt_seeds,
            name_for_user: config.name_for_user,
            first_memory: config.first_memory,
            formality: config.formality,
            extra_instructions: config.extra_instructions,
            relationship: config.relationship,
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
            prompt_seeds: config.prompt_seeds,
            name_for_user: config.name_for_user,
            first_memory: config.first_memory,
            formality: config.formality,
            extra_instructions: config.extra_instructions,
            relationship: config.relationship,
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
                    let template_id = paused.template_id.clone();
                    match self
                        .generate_with_lint(&id, &template_id, &situation_str, &locale, now_utc)
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
            let mut acked_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

            for event in &events {
                match event {
                    OutboxEvent::MessageScheduled {
                        id, template_id, ..
                    } => {
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
            .generate_with_lint(&id, &template_id, &situation_str, &locale, now_utc)
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
                    reason: SkipReason::PausedRateLimit { resume_at: now_utc },
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
        let deliver_body = match self.handle_i18n(&id, &body, &locale, now_utc, 0).await {
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
}
