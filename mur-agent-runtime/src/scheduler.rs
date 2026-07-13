//! C4 — Cron-triggered message injection.
//!
//! `ScheduleEntry.cron` is a 5-field POSIX expression (min hour dom month dow).
//! The `cron` crate requires 6 fields; we prepend `"0 "` (sec=0) at parse time.
//! Each entry runs in its own infinite tokio loop: parse → find next fire →
//! sleep → inject → repeat. All loops are children of `CronScheduler::spawn`,
//! which returns a single `JoinHandle` aborted on SIGTERM by the supervisor.

use crate::llm::{BackgroundKind, RequestIntent};
use crate::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use anyhow::{Context, Result};
use chrono::Local;
use cron::Schedule;
use mur_common::a2a::{Message, MessagePart};
use mur_common::agent::ScheduleEntry;
use std::str::FromStr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Build the `TaskSpec` for a cron-fired injection. Extracted so the
/// `Background(Scheduled)` tagging can be asserted in a unit test without
/// driving the full async cron loop — nobody is synchronously waiting on a
/// cron trigger, so this is an unambiguous background call site.
fn scheduled_task_spec(message: &str) -> TaskSpec {
    TaskSpec {
        input: Message {
            role: "user".into(),
            parts: vec![MessagePart::Text {
                text: message.to_string(),
            }],
        },
        context_task_id: None,
        task_id: None,
        active_fleet: None,
        active_team: None,
        intent: RequestIntent::Background(BackgroundKind::Scheduled),
    }
}

pub struct CronScheduler {
    entries: Vec<ScheduleEntry>,
    runner: Arc<TaskRunner>,
}

impl CronScheduler {
    pub fn new(entries: Vec<ScheduleEntry>, runner: Arc<TaskRunner>) -> Self {
        Self { entries, runner }
    }

    /// Spawn an outer tokio task that fans out one loop per entry.
    /// Each inner loop selects on a shared CancellationToken so aborting
    /// the returned JoinHandle (SIGTERM path in supervisor) cancels all entries.
    /// Push the returned handle onto `supervisor::transport_tasks`.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            let _guard = cancel.clone().drop_guard(); // cancel all entries on abort
            let mut handles = Vec::with_capacity(self.entries.len());
            for entry in self.entries {
                let runner = self.runner.clone();
                let child_cancel = cancel.clone();
                handles.push(tokio::spawn(async move {
                    run_entry(entry, runner, child_cancel).await;
                }));
            }
            // Wait for all inner loops — they exit when cancelled.
            for h in handles {
                let _ = h.await;
            }
        })
    }
}

async fn run_entry(entry: ScheduleEntry, runner: Arc<TaskRunner>, cancel: CancellationToken) {
    let expr = format!("0 {}", entry.cron);
    let schedule = match Schedule::from_str(&expr) {
        Ok(s) => s,
        Err(e) => {
            warn!(cron = %entry.cron, error = %e, "invalid cron expression; entry skipped");
            return;
        }
    };

    loop {
        let now = Local::now();
        let next = match schedule.upcoming(Local).next() {
            Some(t) => t,
            None => {
                warn!(cron = %entry.cron, "cron expression yields no future times; entry disabled");
                return;
            }
        };

        let delta = next - now;

        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = async {
                match delta.to_std() {
                    Ok(dur) => tokio::time::sleep(dur).await,
                    Err(_) => {
                        warn!(cron = %entry.cron, "negative delta after clock adjustment; firing immediately");
                    }
                }
            } => {}
        }

        info!(cron = %entry.cron, message = %entry.message, "cron trigger firing");

        if let Some(ref target) = entry.sends_to {
            warn!(
                sends_to = %target,
                "sends_to cross-agent dispatch not yet implemented; message injected locally"
            );
        }

        let outcome = runner.run_sync(scheduled_task_spec(&entry.message)).await;

        // M2: surface LLM failures in supervisor logs
        if let TaskOutcome::Failed(ref t) = outcome {
            warn!(
                cron = %entry.cron,
                task_id = %t.id,
                "cron-triggered task failed"
            );
        }
    }
}

/// Return the next `count` fire times for a 5-field POSIX cron expression.
///
/// Used by `mur agent schedule next` to preview upcoming firings.
/// Converts 5-field → 6-field by prepending `"0 "` (seconds = 0).
pub fn next_n_fires(cron_expr: &str, count: usize) -> Result<Vec<chrono::DateTime<Local>>> {
    let expr = format!("0 {cron_expr}");
    let schedule = Schedule::from_str(&expr)
        .with_context(|| format!("parse cron expression {cron_expr:?}"))?;
    Ok(schedule.upcoming(Local).take(count).collect())
}

/// Next fire time strictly after `after`, for a 5-field POSIX cron expression
/// (seconds = 0, same grammar as [`next_n_fires`]). `None` when the expression
/// is invalid or yields no future time. Used by the daemon's fleet auto-run to
/// decide cron due-ness in its poll loop (it has a `last_run` lower bound, so it
/// needs "after X" rather than "after now").
pub fn next_fire_after(
    cron_expr: &str,
    after: chrono::DateTime<Local>,
) -> Option<chrono::DateTime<Local>> {
    let expr = format!("0 {cron_expr}");
    Schedule::from_str(&expr).ok()?.after(&after).next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_task_spec_is_tagged_background_scheduled() {
        let spec = scheduled_task_spec("do the thing");
        assert_eq!(
            spec.intent,
            RequestIntent::Background(BackgroundKind::Scheduled),
            "cron-fired turns are unambiguously runtime-initiated — nobody is \
             synchronously waiting on them, so they must route through Smart \
             background candidates rather than the interactive default"
        );
    }
}
