//! Outbox tick loop — Spec §4.8 steps 1, 4, 5, 6, 7.
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
//!   Persistence is deferred to M5.4.
//! - `ledger`: owned; appended to in step 7.
//! - `clock`: `Arc<dyn Clock>` so tests can inject a `MockClock`.
//!
//! Fields that belong to steps 2/3/8–12 (notifier, LLM client, etc.) are
//! deliberately absent; adding them later is additive.

use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDate, Utc};
use mur_common::companion::Situation;
use rand::{RngCore, rngs::StdRng};
use uuid::Uuid;

use crate::companion::{
    clock::Clock,
    earned_permission::{self, BlockReason, GateOutcome},
    picker::{Picker, TemplateId},
    schedule::{self, ScheduleDecision},
    situations,
    telemetry::OutboxEvent,
};
use crate::durable::ledger::Ledger;
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
    /// A `MessageScheduled` event was appended to the ledger.
    Scheduled {
        id: String,
        situation: Situation,
        template_id: TemplateId,
    },
}

/// Why a tick produced no `MessageScheduled` event.
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

    // ── rhythm state ──
    /// Local time of the last successfully scheduled send (updated at step 7).
    pub last_send_at: Option<DateTime<Local>>,
    /// Number of sends already scheduled today (reset on day rollover).
    pub sent_today: u8,
    /// The local date on which a `MorningGreeting` was last scheduled.
    /// `None` means not yet today.  Stored as `Option<NaiveDate>` to match
    /// `situations::pick_for_hour`'s parameter type exactly.
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
    ) -> Self {
        let today = clock.now_local().date_naive();
        Self {
            clock,
            ledger,
            picker,
            proactive,
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
    ) -> Self {
        let today = clock.now_local().date_naive();
        Self {
            clock,
            ledger,
            picker,
            proactive,
            last_send_at: None,
            sent_today: 0,
            morning_sent_today: None,
            today_date: today,
        }
    }

    /// Execute one tick of the outbox loop.
    ///
    /// Steps implemented here: **1, 4, 5, 6, 7**.
    /// Steps 2/3 (resume-paused / passive-dismiss) → M5.5 / M5.6.
    /// Steps 8–12 (LLM / lint / i18n / deliver / finalise) → M5.4 / M5.5.
    pub fn run_tick(&mut self, now_utc: DateTime<Utc>, now_local: DateTime<Local>) -> TickOutcome {
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
        let window_end =
            match schedule::active_window_end_for_today(now_local, self.proactive.quiet_hours.as_ref()) {
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
        // `pick_for_hour` suppresses `morning_greeting` if `morning_sent_today`
        // matches today's date.  We pass the stored Option<NaiveDate> directly.
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
            // Treat as a scheduling failure to avoid double-counts; return
            // NoTemplate-like skip rather than silently claiming success.
            return TickOutcome::Skipped {
                reason: SkipReason::NoTemplate,
            };
        }

        // Update rhythm state.
        self.sent_today += 1;
        self.last_send_at = Some(now_local);
        if situation == Situation::MorningGreeting {
            self.morning_sent_today = Some(today);
        }

        TickOutcome::Scheduled {
            id,
            situation,
            template_id,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use crate::companion::clock::MockClock;
    use crate::companion::picker::{BanditState, Picker, TemplateState};
    use crate::companion::telemetry::OutboxEvent;
    use crate::durable::ledger::Ledger;
    use chrono::{Duration, Local, NaiveDate, NaiveTime, TimeZone};
    use mur_common::agent::{ProactiveConfig, QuietHours};
    use mur_common::companion::Situation;
    use std::sync::Arc;
    use tempfile::TempDir;

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Build a UTC `DateTime` that, when converted to the host's local timezone,
    /// yields the specified `(year, month, day, hour, minute, second)`.
    /// This makes tests timezone-robust: wherever the tests run, the resulting
    /// `MockClock::now_local().hour()` is exactly the requested hour.
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

    /// Build a `BanditState` with one template per situation so `picker.pick`
    /// always has something eligible.
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
                    cooldown_days: 0, // no cooldown → always eligible
                },
            );
        }
        state
    }

    /// Count `MessageScheduled` events in the ledger for the given base dir,
    /// scanning the last 30 days.
    fn count_scheduled(base_dir: &std::path::Path) -> usize {
        Ledger::scan_days::<OutboxEvent>(base_dir, 30)
            .into_iter()
            .filter_map(|r| r.ok())
            .filter(|e| matches!(e, OutboxEvent::MessageScheduled { .. }))
            .count()
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

    // ─────────────────────────────────────────────────────────────────────────
    // Test 1: proactive disabled → 24 h sim yields 0 scheduled
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn proactive_disabled_runs_24h_zero_scheduled() {
        let tmp = TempDir::new().unwrap();
        // Start at local 08:00 so the clock is mid-morning regardless of TZ.
        // quiet_hours are disabled so the only gate is `enabled`.
        let base_utc = local_as_utc(2026, 4, 29, 8, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 42);
        let proactive = make_proactive(false, 3, None, None);
        let mut outbox = Outbox::with_picker(clock.clone(), ledger, picker, proactive);

        let ticks = 24 * 60; // 1 tick per minute × 1440 min = 24 h
        for _ in 0..ticks {
            let now_utc = clock.now_utc();
            let now_local = clock.now_local();
            let outcome = outbox.run_tick(now_utc, now_local);
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

    // ─────────────────────────────────────────────────────────────────────────
    // Test 2: enabled, cap=3, 12 h window → exactly 3 scheduled
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn enabled_cap3_window12h_yields_exactly_3_scheduled() {
        let tmp = TempDir::new().unwrap();
        // Start at local 10:00.  Quiet hours: 22:00–23:59 → active window 10:00–22:00 = 12 h.
        // Using local_as_utc so MockClock::now_local() returns exactly 10:00 local.
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 99);
        // quiet_hours start=22:00 → window ends at local 22:00; 10:00→22:00 = 12 h
        let proactive = make_proactive(true, 3, Some("22:00"), Some("23:59"));
        let mut outbox = Outbox::with_picker(clock.clone(), ledger, picker, proactive);

        let ticks = 12 * 60; // 720 ticks at 1/min
        let mut scheduled_count = 0usize;
        for _ in 0..ticks {
            let now_utc = clock.now_utc();
            let now_local = clock.now_local();
            let outcome = outbox.run_tick(now_utc, now_local);
            if matches!(outcome, TickOutcome::Scheduled { .. }) {
                scheduled_count += 1;
            }
            clock.advance(Duration::seconds(60));
        }

        assert_eq!(
            scheduled_count, 3,
            "expected exactly 3 Scheduled outcomes across a 12h window with cap=3"
        );
        assert_eq!(
            count_scheduled(tmp.path()),
            3,
            "ledger must have exactly 3 MessageScheduled events"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Test 3: morning_greeting only fires once per day
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn morning_greeting_only_once_per_day() {
        let tmp = TempDir::new().unwrap();
        // Start at local 06:30 — hour 6 → weights_by_hour returns morning_greeting
        // with weight 0.6.  Using local_as_utc so MockClock::now_local().hour() == 6
        // regardless of the host's timezone.
        let base_utc = local_as_utc(2026, 4, 29, 6, 30, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        // seed=0 → StdRng deterministic; with weight [0.6, 0, 0.4, 0] at hour 6,
        // the first eligible pick will be MorningGreeting.
        let picker = Picker::with_seed(seed_bandit_state(), 0);
        // No quiet hours, high cap so we can observe multiple sends.
        let proactive = make_proactive(true, 10, None, None);
        let mut outbox = Outbox::with_picker(clock.clone(), ledger, picker, proactive);

        let mut morning_count = 0usize;
        // Run for 4 h (hours 6–10) at 5-min intervals = 48 ticks.
        for _ in 0..48 {
            let now_utc = clock.now_utc();
            let now_local = clock.now_local();
            if let TickOutcome::Scheduled { situation, .. } = outbox.run_tick(now_utc, now_local) {
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

    // ─────────────────────────────────────────────────────────────────────────
    // Test 4: day rollover resets counters
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn day_rollover_resets_counters() {
        let tmp = TempDir::new().unwrap();
        // Start at local 10:00 on day N.  Active window 10:00–22:00 (quiet 22:00+).
        let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
        let clock = Arc::new(MockClock::at(base_utc));
        let ledger = Ledger::open(tmp.path()).unwrap();
        let picker = Picker::with_seed(seed_bandit_state(), 7);
        // cap=1 → only 1 send per day
        let proactive = make_proactive(true, 1, Some("22:00"), Some("23:59"));
        let mut outbox = Outbox::with_picker(clock.clone(), ledger, picker, proactive);

        // ── Day N: first tick should schedule ─────────────────────────────
        let outcome_day_n = outbox.run_tick(clock.now_utc(), clock.now_local());
        assert!(
            matches!(outcome_day_n, TickOutcome::Scheduled { .. }),
            "day N first tick should schedule; got {outcome_day_n:?}"
        );
        assert_eq!(outbox.sent_today, 1);

        // Advance 25 h → day N+1, 11:00 UTC
        clock.advance(Duration::hours(25));

        // ── Day N+1: rollover should reset sent_today, and a new send fires ──
        let outcome_day_n1 = outbox.run_tick(clock.now_utc(), clock.now_local());
        assert_eq!(
            outbox.sent_today, 1,
            "after rollover sent_today should be 1 (just incremented from 0)"
        );
        assert!(
            matches!(outcome_day_n1, TickOutcome::Scheduled { .. }),
            "day N+1 first tick should schedule after rollover; got {outcome_day_n1:?}"
        );

        // Two MessageScheduled events total (one per day).
        assert_eq!(
            count_scheduled(tmp.path()),
            2,
            "ledger should have 2 MessageScheduled events (one per day)"
        );
    }
}
