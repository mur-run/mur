//! C6 — Idle-triggered message injection.
//!
//! `IdleScheduler` wakes every `tick_interval` (default 30s) and checks each
//! `IdleTrigger` for eligibility via the pure `should_fire` function:
//!   1. `(now − last_activity) >= after_secs` — agent has been idle long enough
//!   2. `(now − last_fire) >= cooldown_secs` — cooldown has elapsed since last fire
//!   3. `now < quiet_window_end` (when `respect_quiet_hours` is true)
//!
//! When all gates pass the trigger message is injected into the `TaskRunner`.

use crate::companion::schedule::active_window_end_for_today;
use crate::llm::{BackgroundKind, RequestIntent};
use crate::task_runner::{TaskRunner, TaskSpec};
use chrono::Local;
use mur_common::a2a::{Message, MessagePart};
use mur_common::agent::{IdleTrigger, QuietHours};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const DEFAULT_TICK_SECS: u64 = 30;

/// Pure eligibility check — no I/O.
///
/// - `now_secs`: current Unix timestamp
/// - `last_activity_secs`: Unix timestamp of last task start; 0 means "never started"
/// - `last_fire_secs`: Unix timestamp of last fire for this trigger; 0 means "never fired"
/// - `quiet_window_end_secs`: if `Some(t)` and `respect_quiet_hours`, fires only when `now < t`
pub fn should_fire(
    trigger: &IdleTrigger,
    now_secs: i64,
    last_activity_secs: i64,
    last_fire_secs: i64,
    quiet_window_end_secs: Option<i64>,
) -> bool {
    let idle_elapsed = now_secs.saturating_sub(last_activity_secs);
    if idle_elapsed < trigger.after_secs as i64 {
        return false;
    }
    if last_fire_secs != 0 {
        let cooldown_elapsed = now_secs.saturating_sub(last_fire_secs);
        if cooldown_elapsed < trigger.cooldown_secs as i64 {
            return false;
        }
    }
    if trigger.respect_quiet_hours
        && let Some(window_end) = quiet_window_end_secs
        && now_secs >= window_end
    {
        return false;
    }
    true
}

pub struct IdleScheduler {
    triggers: Vec<IdleTrigger>,
    runner: Arc<TaskRunner>,
    quiet_hours: Option<QuietHours>,
    tick_interval: Duration,
}

impl IdleScheduler {
    pub fn new(
        triggers: Vec<IdleTrigger>,
        runner: Arc<TaskRunner>,
        quiet_hours: Option<QuietHours>,
    ) -> Self {
        Self {
            triggers,
            runner,
            quiet_hours,
            tick_interval: Duration::from_secs(DEFAULT_TICK_SECS),
        }
    }

    /// Override tick interval — used by tests and the supervisor smoke harness.
    pub fn with_tick_interval(mut self, d: Duration) -> Self {
        self.tick_interval = d;
        self
    }

    /// Spawn the idle-check loop. Returns a `JoinHandle` that can be aborted on SIGTERM.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            let _guard = cancel.clone().drop_guard();
            run_idle_loop(self, cancel).await;
        })
    }
}

async fn run_idle_loop(scheduler: IdleScheduler, cancel: CancellationToken) {
    let mut last_fire: Vec<i64> = vec![0; scheduler.triggers.len()];

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(scheduler.tick_interval) => {}
        }

        let now_local = Local::now();
        let now_secs = now_local.timestamp();
        let last_activity_secs = scheduler.runner.last_activity_at();
        let quiet_window_end_secs =
            active_window_end_for_today(now_local, scheduler.quiet_hours.as_ref())
                .map(|dt| dt.timestamp());

        for (i, trigger) in scheduler.triggers.iter().enumerate() {
            if should_fire(
                trigger,
                now_secs,
                last_activity_secs,
                last_fire[i],
                quiet_window_end_secs,
            ) {
                info!(
                    after_secs = trigger.after_secs,
                    message = %trigger.message,
                    "idle trigger firing"
                );

                if let Some(ref target) = trigger.sends_to {
                    warn!(
                        sends_to = %target,
                        "sends_to cross-agent dispatch not yet implemented; message injected locally"
                    );
                }

                let input = Message {
                    role: "user".into(),
                    parts: vec![MessagePart::Text {
                        text: trigger.message.clone(),
                    }],
                };
                let _outcome = scheduler
                    .runner
                    .run_sync(TaskSpec {
                        input,
                        context_task_id: None,
                        task_id: None,
                        active_fleet: None,
                        active_team: None,
                        // Idle triggers are runtime-initiated maintenance nudges —
                        // no live user is watching this turn.
                        intent: RequestIntent::Background(BackgroundKind::Maintenance),
                    })
                    .await;

                last_fire[i] = now_secs;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(after_secs: u64, cooldown_secs: u64, respect_quiet: bool) -> IdleTrigger {
        IdleTrigger {
            after_secs,
            message: "test".into(),
            sends_to: None,
            cooldown_secs,
            respect_quiet_hours: respect_quiet,
        }
    }

    #[test]
    fn not_idle_enough() {
        let t = trigger(3600, 600, false);
        assert!(!should_fire(&t, 1000, 900, 0, None));
    }

    #[test]
    fn idle_enough_no_prior_fire() {
        let t = trigger(3600, 600, false);
        assert!(should_fire(&t, 5000, 1000, 0, None));
    }

    #[test]
    fn cooldown_blocks_refire() {
        let t = trigger(100, 600, false);
        // 200s idle, last fired 100s ago — cooldown (600s) not elapsed
        assert!(!should_fire(&t, 1000, 800, 900, None));
    }

    #[test]
    fn quiet_window_blocks_when_past_end() {
        let t = trigger(100, 0, true);
        // idle enough, but quiet window ended 1s ago
        assert!(!should_fire(&t, 1000, 800, 0, Some(999)));
    }
}
