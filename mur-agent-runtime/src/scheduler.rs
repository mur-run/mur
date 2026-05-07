//! C4 — Cron-triggered message injection.
//!
//! `ScheduleEntry.cron` is a 5-field POSIX expression (min hour dom month dow).
//! The `cron` crate requires 6 fields; we prepend `"0 "` (sec=0) at parse time.
//! Each entry runs in its own infinite tokio loop: parse → find next fire →
//! sleep → inject → repeat. All loops are children of `CronScheduler::spawn`,
//! which returns a single `JoinHandle` aborted on SIGTERM by the supervisor.

use crate::task_runner::{TaskRunner, TaskSpec};
use anyhow::{Context, Result};
use chrono::Local;
use cron::Schedule;
use mur_common::a2a::{Message, MessagePart};
use mur_common::agent::ScheduleEntry;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{info, warn};

pub struct CronScheduler {
    entries: Vec<ScheduleEntry>,
    runner: Arc<TaskRunner>,
}

impl CronScheduler {
    pub fn new(entries: Vec<ScheduleEntry>, runner: Arc<TaskRunner>) -> Self {
        Self { entries, runner }
    }

    /// Spawn an outer tokio task that fans out one loop per entry.
    /// Push the returned handle onto `supervisor::transport_tasks` so it is
    /// aborted on SIGTERM alongside all other transports.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut handles = Vec::with_capacity(self.entries.len());
            for entry in self.entries {
                let runner = self.runner.clone();
                handles.push(tokio::spawn(async move {
                    run_entry(entry, runner).await;
                }));
            }
            for h in handles {
                let _ = h.await;
            }
        })
    }
}

async fn run_entry(entry: ScheduleEntry, runner: Arc<TaskRunner>) {
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
        if delta.num_milliseconds() > 0 {
            if let Ok(dur) = delta.to_std() {
                tokio::time::sleep(dur).await;
            }
        }

        info!(cron = %entry.cron, message = %entry.message, "cron trigger firing");

        if let Some(ref target) = entry.sends_to {
            // Cross-agent dispatch deferred to C4 v2; log and fall through to local.
            warn!(
                sends_to = %target,
                "sends_to cross-agent dispatch not yet implemented; message injected locally"
            );
        }

        let input = Message {
            role: "user".into(),
            parts: vec![MessagePart::Text {
                text: entry.message.clone(),
            }],
        };
        runner
            .run_sync(TaskSpec {
                input,
                context_task_id: None,
            })
            .await;
    }
}

/// Return the next `count` fire times for a 5-field POSIX cron expression.
///
/// Used by `mur agent schedule next` to preview upcoming firings.
/// Converts 5-field → 6-field by prepending `"0 "` (seconds = 0).
pub fn next_n_fires(
    cron_expr: &str,
    count: usize,
) -> Result<Vec<chrono::DateTime<Local>>> {
    let expr = format!("0 {cron_expr}");
    let schedule = Schedule::from_str(&expr)
        .with_context(|| format!("parse cron expression {cron_expr:?}"))?;
    Ok(schedule.upcoming(Local).take(count).collect())
}
