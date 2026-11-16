//! #2436 — a one-off entry whose firing passed while the runtime was down.
//!
//! Before this, such an entry was retired on the next start with nothing but an
//! `info!` line: the reminder was lost and the person who set it had no way to
//! notice short of reading the schedule channel by hand.
//!
//! The scheduler cannot write `profile.yaml` (`SELF_PROTECTED_AGENT_FILES`,
//! #712), so "did this fire?" is answered from the schedule channel, which it
//! can write: a fired entry leaves a reply keyed `sched:<task_id>:<cron>`, and a
//! reported miss leaves one keyed by [`missed_key`]. Either one means there is
//! nothing to report.
//!
//! A notice, not a late firing: the person learns the reminder was missed and
//! reads its text, and nothing runs on their behalf hours or days after the
//! moment they chose.

use super::{ScheduleSink, notify_inbox, schedule_channel};
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use mur_channel::ChannelService;
use mur_common::agent::ScheduleEntry;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use tracing::{info, warn};

/// Idempotency key of the notice for one entry's one firing. Stable across
/// restarts, so the channel's own dedup makes the notice exactly-once.
pub(super) fn missed_key(cron: &str, bound: DateTime<Local>) -> String {
    // UTC, so a change of the machine's zone cannot mint a second key.
    format!(
        "sched-missed:{cron}:{}",
        bound.with_timezone(&Utc).to_rfc3339()
    )
}

/// Whether the channel already accounts for the firing at `bound`: a reply
/// recorded at or after it, or a notice already posted for it.
///
/// The `ts >= bound` guard matters because cron has no year: the same
/// expression may have fired in an earlier year, and that reply says nothing
/// about this one.
pub(super) fn already_accounted(
    events: &[ChannelEvent],
    cron: &str,
    bound: DateTime<Local>,
) -> bool {
    let notice = missed_key(cron, bound);
    let reply_suffix = format!(":{cron}");
    let bound_utc = bound.with_timezone(&Utc);
    events.iter().any(|e| match e.idempotency_key.as_deref() {
        Some(k) if k == notice => true,
        Some(k) => k.starts_with("sched:") && k.ends_with(&reply_suffix) && e.ts >= bound_utc,
        None => false,
    })
}

pub(super) fn notice_text(message: &str, bound: DateTime<Local>) -> String {
    format!(
        "Missed reminder: due {}, but no firing of it was recorded — the agent was \
         most likely not running then. The reminder was:\n\n{message}",
        bound.format("%Y-%m-%d %H:%M %:z")
    )
}

/// Post the missed-firing notice to the schedule channel and the inbox, once.
///
/// The inbox write follows only a fresh channel write. The channel is what
/// dedups; without it every restart would raise the same notice again, so a
/// channel failure skips the inbox rather than risk repeating it forever.
///
/// Returns whether a notice was posted by this call.
pub(super) async fn report_missed(
    sink: &ScheduleSink,
    entry: &ScheduleEntry,
    bound: DateTime<Local>,
    now: DateTime<Utc>,
) -> bool {
    let text = notice_text(&entry.message, bound);
    let post = || -> Result<bool> {
        let svc = ChannelService::open(&sink.mur_home)?;
        let channel_id = schedule_channel(&svc, &sink.mur_home, &sink.agent)?;
        if already_accounted(&svc.load_events(&channel_id)?, &entry.cron, bound) {
            return Ok(false);
        }
        svc.append_signed(
            &channel_id,
            &sink.identity,
            sink.key_version,
            ChannelActor::Agent {
                id: sink.agent.clone(),
            },
            EventKind::Message,
            serde_json::json!({ "text": text }),
            Some(missed_key(&entry.cron, bound)),
        )?;
        Ok(true)
    };
    match post() {
        Ok(false) => false,
        Ok(true) => {
            info!(cron = %entry.cron, due = %bound.to_rfc3339(), "missed one-off schedule reported");
            let id = uuid::Uuid::now_v7().to_string();
            notify_inbox(sink, &id, &text, now).await;
            true
        }
        Err(e) => {
            warn!(
                error = %e, cron = %entry.cron, due = %bound.to_rfc3339(),
                "a one-off schedule was missed and the notice could not be recorded"
            );
            false
        }
    }
}

#[cfg(test)]
#[path = "missed_tests.rs"]
mod tests;
