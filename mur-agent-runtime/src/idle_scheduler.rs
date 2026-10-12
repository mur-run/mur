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

/// Where the scheduler reads its triggers from: a fixed list, or a live read
/// of `profile.yaml`'s `lifecycle.idle_triggers`, re-read every tick.
pub type TriggerSource = Arc<dyn Fn() -> Vec<IdleTrigger> + Send + Sync>;

pub struct IdleScheduler {
    source: TriggerSource,
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
        Self::live(Arc::new(move || triggers.clone()), runner, quiet_hours)
    }

    /// Triggers re-read from `source` on every tick.
    pub fn live(
        source: TriggerSource,
        runner: Arc<TaskRunner>,
        quiet_hours: Option<QuietHours>,
    ) -> Self {
        Self {
            source,
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
    // Each trigger with the time it last fired. Carried across re-reads by
    // value, so an edit to one trigger never resets another's cooldown — a
    // reset would let every trigger fire again on the tick after any edit.
    let mut fired: Vec<(IdleTrigger, i64)> = Vec::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(scheduler.tick_interval) => {}
        }

        fired = carry_last_fire(fired, (scheduler.source)());
        let now_local = Local::now();
        let now_secs = now_local.timestamp();
        let last_activity_secs = scheduler.runner.last_activity_at();
        let quiet_window_end_secs =
            active_window_end_for_today(now_local, scheduler.quiet_hours.as_ref())
                .map(|dt| dt.timestamp());

        for (trigger, last_fire) in fired.iter_mut() {
            if should_fire(
                trigger,
                now_secs,
                last_activity_secs,
                *last_fire,
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
                        cwd: None,
                        input,
                        context_task_id: None,
                        task_id: None,
                        active_fleet: None,
                        active_team: None,
                        // Idle triggers are runtime-initiated maintenance nudges —
                        // no live user is watching this turn.
                        intent: RequestIntent::Background(BackgroundKind::Maintenance),
                        output_artifact_path: None,
                        attended: false,
                        deadline_secs: None,
                    })
                    .await;

                *last_fire = now_secs;
            }
        }
    }
}

/// The new trigger list, each paired with the last-fire time of an equal
/// trigger from the previous list (0 = never). Matched one-for-one, so two
/// identical triggers keep two cooldowns.
fn carry_last_fire(
    prev: Vec<(IdleTrigger, i64)>,
    wanted: Vec<IdleTrigger>,
) -> Vec<(IdleTrigger, i64)> {
    let mut prev: Vec<Option<(IdleTrigger, i64)>> = prev.into_iter().map(Some).collect();
    wanted
        .into_iter()
        .map(|t| {
            let at = prev
                .iter_mut()
                .find(|p| p.as_ref().is_some_and(|(o, _)| *o == t))
                .and_then(Option::take)
                .map_or(0, |(_, at)| at);
            (t, at)
        })
        .collect()
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

    #[test]
    fn an_edit_keeps_the_untouched_triggers_cooldown() {
        let a = trigger(100, 600, false);
        let mut b = trigger(200, 600, false);
        let prev = vec![(a.clone(), 900), (b.clone(), 800)];
        b.message = "edited".into();
        let next = carry_last_fire(prev, vec![a.clone(), b.clone(), trigger(300, 0, false)]);
        assert_eq!(next[0], (a, 900), "unchanged keeps its cooldown");
        assert_eq!(next[1], (b, 0), "an edited trigger is a new one");
        assert_eq!(next[2].1, 0, "an added trigger has never fired");
    }

    #[test]
    fn identical_triggers_keep_their_own_cooldowns() {
        let a = trigger(100, 600, false);
        let next = carry_last_fire(vec![(a.clone(), 1), (a.clone(), 2)], vec![a.clone(), a]);
        assert_eq!(next.iter().map(|(_, t)| *t).collect::<Vec<_>>(), vec![1, 2]);
    }
}
