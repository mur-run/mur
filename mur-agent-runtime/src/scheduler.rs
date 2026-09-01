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
use chrono::{DateTime, Local};
use cron::Schedule;
use mur_channel::ChannelService;
use mur_common::a2a::{Message, MessagePart};
use mur_common::agent::{SCHEDULE_CHANNEL_FILE, ScheduleEntry};
use mur_common::identity::AgentIdentity;
use std::path::{Path, PathBuf};
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
        output_artifact_path: None,
    }
}

/// Whether a bounded entry has run out of firings — its next occurrence falls
/// after the bound. `None` means unbounded, the shape every recurring entry has.
///
/// Extracted from the async loop for the same reason [`scheduled_task_spec`] is:
/// nothing else can assert the boundary without driving a real cron wait.
///
fn bound_exhausted(next: DateTime<Local>, not_after: Option<&str>) -> bool {
    parse_bound(not_after).is_some_and(|b| next > b)
}

/// A `not_after` bound as an instant — `None` for both "absent" and "does not
/// parse".
///
/// Public because every surface that renders a schedule must read the bound the
/// same way the scheduler does. Sharing this function makes that structural: a
/// second parser could drift, and the drift would show an entry as finished on
/// the Panel while the scheduler kept firing it.
///
/// Unparseable folding into `None` is deliberate — the two ways to be wrong are
/// not symmetric. Ignoring a corrupt bound lets a one-shot repeat, which is
/// visible in `mur agent schedule list` and one `remove` away. Honouring it
/// would retire the entry, and a reminder that silently never fires is the
/// failure the user has no way to notice.
pub fn parse_bound(not_after: Option<&str>) -> Option<DateTime<Local>> {
    let raw = not_after?;
    match DateTime::parse_from_rfc3339(raw) {
        Ok(b) => Some(b.with_timezone(&Local)),
        Err(e) => {
            warn!(
                not_after = %raw, error = %e,
                "not_after is not RFC3339; treating the entry as unbounded"
            );
            None
        }
    }
}

/// Where a fired entry leaves its reply.
///
/// Before #1125 a completed scheduled turn was discarded: the scheduler warned
/// on failure and did nothing at all with success, so a reminder fired on the
/// second and reached nobody. The reply is now appended to a channel, which the
/// Hub, the Panel and `mur channel` already read — one write, every surface.
pub struct ScheduleSink {
    pub mur_home: PathBuf,
    pub agent: String,
    pub identity: Arc<AgentIdentity>,
    pub key_version: u32,
}

/// The channel a fired entry writes to — created once, then remembered in the
/// agent's home.
///
/// Deliberately not `ChannelService::latest_for_agent`: that returns whatever
/// conversation the agent last took part in, so a breakfast reminder would land
/// in the middle of an unrelated fleet thread, and in a different thread each
/// time it fired. A remembered id keeps every firing in one findable place.
///
/// A marker naming a channel that no longer loads is replaced rather than
/// treated as an error — deleting a channel must not silently stop an agent's
/// schedules from being recorded anywhere.
fn schedule_channel(svc: &ChannelService, mur_home: &Path, agent: &str) -> Result<String> {
    let marker = mur_home
        .join("agents")
        .join(agent)
        .join(SCHEDULE_CHANNEL_FILE);
    if let Ok(raw) = std::fs::read_to_string(&marker) {
        let id = raw.trim();
        if !id.is_empty() && svc.exists(id) {
            return Ok(id.to_string());
        }
    }
    let ch = svc.create_for_agent(agent)?;
    if let Some(dir) = marker.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&marker, &ch.id).context("record the schedule channel id")?;
    Ok(ch.id)
}

/// Append a completed scheduled turn's reply to the agent's schedule channel.
///
/// Best-effort, like the `channel/delegate` write it mirrors: a schedule that
/// fired must not be reported as failed because the channel store was busy.
fn record_reply(sink: &ScheduleSink, task: &mur_common::a2a::Task, cron: &str) {
    let text = crate::protocol::methods::channel_delegate::reply_text_of(task);
    if text.trim().is_empty() {
        return;
    }
    let write = || -> Result<()> {
        let svc = ChannelService::open(&sink.mur_home)?;
        let channel_id = schedule_channel(&svc, &sink.mur_home, &sink.agent)?;
        crate::protocol::methods::channel_delegate::append_self_reply(
            &sink.mur_home,
            &channel_id,
            &sink.agent,
            &sink.identity,
            sink.key_version,
            &text,
            &task.id,
            Some(format!("sched:{}:{}", task.id, cron)),
        )
    };
    if let Err(e) = write() {
        warn!(
            error = %e, cron = %cron, task_id = %task.id,
            "cron-triggered turn completed but its reply could not be recorded"
        );
    }
}

pub struct CronScheduler {
    entries: Vec<ScheduleEntry>,
    runner: Arc<TaskRunner>,
    /// `None` leaves the pre-#1125 behaviour — a fired turn is run and its
    /// reply discarded. Only tests construct the scheduler that way.
    sink: Option<Arc<ScheduleSink>>,
}

impl CronScheduler {
    pub fn new(entries: Vec<ScheduleEntry>, runner: Arc<TaskRunner>) -> Self {
        Self {
            entries,
            runner,
            sink: None,
        }
    }

    /// Record each fired turn's reply into the agent's schedule channel.
    pub fn with_sink(mut self, sink: ScheduleSink) -> Self {
        self.sink = Some(Arc::new(sink));
        self
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
                let sink = self.sink.clone();
                handles.push(tokio::spawn(async move {
                    run_entry(entry, runner, child_cancel, sink).await;
                }));
            }
            // Wait for all inner loops — they exit when cancelled.
            for h in handles {
                let _ = h.await;
            }
        })
    }
}

async fn run_entry(
    entry: ScheduleEntry,
    runner: Arc<TaskRunner>,
    cancel: CancellationToken,
    sink: Option<Arc<ScheduleSink>>,
) {
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

        if bound_exhausted(next, entry.not_after.as_deref()) {
            info!(
                cron = %entry.cron,
                not_after = entry.not_after.as_deref().unwrap_or_default(),
                "schedule retired — its last firing is past, so it will not repeat"
            );
            return;
        }

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

        match &outcome {
            // M2: surface LLM failures in supervisor logs
            TaskOutcome::Failed(t) => warn!(
                cron = %entry.cron,
                task_id = %t.id,
                "cron-triggered task failed"
            ),
            // #1125: a completed turn used to end here, which is how a reminder
            // fired on the second and reached nobody.
            TaskOutcome::Completed(t) => {
                if let Some(sink) = sink.as_deref() {
                    record_reply(sink, t, &entry.cron);
                }
            }
            TaskOutcome::Cancelled(_) => {}
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

    fn at(s: &str) -> DateTime<Local> {
        DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&Local)
    }

    #[test]
    fn an_unbounded_entry_never_retires() {
        assert!(
            !bound_exhausted(at("2099-01-01T00:00:00+08:00"), None),
            "a recurring entry carries no bound and must keep firing forever"
        );
    }

    #[test]
    fn the_firing_a_bound_names_is_still_allowed() {
        // The boundary case that makes a one-shot fire at all: `not_after` is
        // set to the entry's own first firing, so that firing must be admitted
        // and only the NEXT one retired. An off-by-one here turns every
        // one-shot reminder into one that never arrives.
        let fire = "2026-09-01T10:00:00+08:00";
        assert!(
            !bound_exhausted(at(fire), Some(fire)),
            "the firing the bound names must happen"
        );
    }

    #[test]
    fn the_firing_after_the_bound_retires_the_entry() {
        assert!(
            bound_exhausted(
                at("2027-09-01T10:00:00+08:00"),
                Some("2026-09-01T10:00:00+08:00")
            ),
            "cron has no year, so a dated request recurs annually — the bound is \
             the only thing that stops next September from firing too (#1119)"
        );
    }

    #[test]
    fn the_schedule_channel_is_created_once_and_remembered() {
        // Every firing must land in the same place. Creating a channel per
        // firing would bury a daily reminder under a new thread each morning,
        // which is why this does not just call `create_for_agent` (#1125).
        let tmp = tempfile::tempdir().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let first = schedule_channel(&svc, tmp.path(), "probe").unwrap();
        let second = schedule_channel(&svc, tmp.path(), "probe").unwrap();
        assert_eq!(first, second, "the second firing must reuse the channel");
        assert_eq!(
            std::fs::read_to_string(
                tmp.path()
                    .join("agents")
                    .join("probe")
                    .join(SCHEDULE_CHANNEL_FILE)
            )
            .unwrap()
            .trim(),
            first,
            "the marker records the channel it handed out"
        );
    }

    #[test]
    fn a_marker_naming_a_channel_that_is_gone_is_replaced() {
        // Deleting a channel must not permanently stop an agent's schedules
        // from being recorded anywhere — the marker is a cache, not a claim.
        let tmp = tempfile::tempdir().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let dir = tmp.path().join("agents").join("probe");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SCHEDULE_CHANNEL_FILE), "no-such-channel").unwrap();

        let id = schedule_channel(&svc, tmp.path(), "probe").unwrap();
        assert_ne!(id, "no-such-channel", "a dead id must not be handed back");
        assert!(svc.exists(&id), "the replacement must be real");
    }

    #[test]
    fn an_unparseable_bound_leaves_the_entry_firing() {
        assert!(
            !bound_exhausted(at("2099-01-01T00:00:00+08:00"), Some("next tuesday")),
            "a corrupt bound must not silently retire a reminder: a repeat is \
             visible and removable, a reminder that never fires is not"
        );
    }
}
