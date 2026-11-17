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
use mur_channel::{ChannelService, ChannelStore};
use mur_common::agent::{SCHEDULE_CHANNEL_FILE, ScheduleEntry};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use sha2::{Digest, Sha256};
use std::path::Path;
use tracing::{info, warn};

/// Identity of one entry, from everything that makes it a different reminder.
///
/// The cron alone is not one: `schedule add` takes any number of entries with
/// the same expression and different messages, and keying on it let one
/// reminder's firing or notice silence another due the same minute. A content
/// hash rather than a stored id, because the scheduler cannot write the
/// profile to mint one, and identical entries are one reminder anyway.
pub(super) fn entry_id(entry: &ScheduleEntry) -> String {
    let mut h = Sha256::new();
    for field in [
        entry.cron.as_str(),
        entry.message.as_str(),
        entry.sends_to.as_deref().unwrap_or_default(),
        entry.not_after.as_deref().unwrap_or_default(),
    ] {
        // Length-prefixed, so ("ab", "c") and ("a", "bc") stay distinct.
        h.update((field.len() as u64).to_le_bytes());
        h.update(field.as_bytes());
    }
    hex::encode(&h.finalize()[..ENTRY_ID_BYTES])
}

/// 64 bits: collisions need two entries on one agent, not a public namespace.
const ENTRY_ID_BYTES: usize = 8;

/// Idempotency key of a completed firing's reply.
pub(super) fn reply_key(task_id: &str, entry: &ScheduleEntry) -> String {
    format!("sched:{task_id}:{}#{}", entry.cron, entry_id(entry))
}

/// Idempotency key of the notice for one entry's one firing. Stable across
/// restarts, so the channel's own dedup makes the notice exactly-once.
pub(super) fn missed_key(entry: &ScheduleEntry, bound: DateTime<Local>) -> String {
    // UTC, so a change of the machine's zone cannot mint a second key.
    format!(
        "sched-missed:{}#{}:{}",
        entry.cron,
        entry_id(entry),
        bound.with_timezone(&Utc).to_rfc3339()
    )
}

/// What the schedule channel says about one entry's firing at `bound`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FiringOutcome {
    /// A completed firing left its reply at or after the bound.
    Fired,
    /// The runtime found the firing passed while it was down and posted a notice.
    Missed,
    /// Neither: the runtime has not started since the bound, or the turn
    /// fired with an empty reply (which records nothing). Never read as missed.
    NoRecord,
}

/// Read the channel's verdict on the firing at `bound`.
///
/// A reply counts only if recorded at or after the bound, because cron has no
/// year: the same expression may have fired in an earlier year, and that
/// reply says nothing about this one.
///
/// A reply keyed by the bare cron predates entry ids and still counts. It
/// cannot say which same-minute entry fired, but treating it as unaccounted
/// would raise a false "missed" for every reminder that fired before upgrade.
pub fn firing_outcome(
    events: &[ChannelEvent],
    entry: &ScheduleEntry,
    bound: DateTime<Local>,
) -> FiringOutcome {
    let notice = missed_key(entry, bound);
    let reply_suffix = format!(":{}#{}", entry.cron, entry_id(entry));
    let legacy_suffix = format!(":{}", entry.cron);
    let bound_utc = bound.with_timezone(&Utc);
    let mut missed = false;
    for e in events {
        match e.idempotency_key.as_deref() {
            // A reply beats a notice: if both exist, it did fire.
            Some(k)
                if k.starts_with("sched:")
                    && (k.ends_with(&reply_suffix) || k.ends_with(&legacy_suffix))
                    && e.ts >= bound_utc =>
            {
                return FiringOutcome::Fired;
            }
            Some(k) if k == notice => missed = true,
            _ => {}
        }
    }
    if missed {
        FiringOutcome::Missed
    } else {
        FiringOutcome::NoRecord
    }
}

/// Whether the channel already accounts for the firing at `bound`: a reply
/// recorded at or after it, or a notice already posted for it.
pub(super) fn already_accounted(
    events: &[ChannelEvent],
    entry: &ScheduleEntry,
    bound: DateTime<Local>,
) -> bool {
    firing_outcome(events, entry, bound) != FiringOutcome::NoRecord
}

/// [`firing_outcome`] for `agent` under `mur_home`, read without side effects.
///
/// For host-side readers (`schedule list`, the Panel): it never creates the
/// schedule channel or its marker the way the runtime's writer path does. No
/// channel yet means nothing was ever recorded.
pub fn read_firing_outcome(
    mur_home: &Path,
    agent: &str,
    entry: &ScheduleEntry,
    bound: DateTime<Local>,
) -> Result<FiringOutcome> {
    let marker = mur_home
        .join("agents")
        .join(agent)
        .join(SCHEDULE_CHANNEL_FILE);
    let id = match std::fs::read_to_string(&marker) {
        Ok(raw) => raw.trim().to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FiringOutcome::NoRecord);
        }
        Err(e) => return Err(e.into()),
    };
    if id.is_empty() {
        return Ok(FiringOutcome::NoRecord);
    }
    let events = ChannelStore::new(mur_home).load_events(&id)?;
    Ok(firing_outcome(&events, entry, bound))
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
        if already_accounted(&svc.load_events(&channel_id)?, entry, bound) {
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
            Some(missed_key(entry, bound)),
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
