//! §7.0 derived session state, shared by prune, resume and `fleet delete`
//! so all three agree on what a review session is. State is never stored:
//! it is read from the run lock, the fleet definition, the channel's last
//! review events and the §8.2 corrupted marker.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_channel::ChannelService;
use serde::Deserialize;

use super::constants::REVIEW_CORRUPTED_MARKER_FILE;
use super::ledger::{Ledger, fold_rounds};
use super::run_lock::{self, DriverLock, LockDenied};
use super::schema::{Cumulative, NoteClassification, ReviewPayload, classify_note_payload};
use crate::cmd::fleet::store;

/// §7.0 states. `Running` carries the holder description for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Running(Option<String>),
    Paused,
    /// Lock free, definition present, last event neither `paused` nor
    /// `session_stopped`. Also covers "stopped, but the definition was left
    /// behind" (a driver that died between the two steps of ending), so that
    /// combination still has an exit through `--include-paused`.
    Crashed,
    Stopped,
    Corrupted,
    /// A marker exists but cannot be read: listed, never acted on (§8.2).
    CorruptedUnreadable(PathBuf),
    Orphaned,
}

/// The §8.2 marker, read-only here.
#[derive(Debug, Deserialize)]
struct CorruptedMarker {
    detected_at: DateTime<Utc>,
}

/// One observed session. `lock` is held when the state is not `Running`,
/// so a caller that acts on the session does so under the run lock.
#[derive(Debug)]
pub struct Observed {
    pub state: SessionState,
    /// Later of the last readable event and the marker's `detected_at`;
    /// the channel's creation time when it has no events at all.
    pub last: Option<DateTime<Utc>>,
    pub lock: Option<DriverLock>,
}

/// Classify `session` (channel `channel_id`). Takes the run lock when it is
/// free; a lock that cannot be taken at all is an error, never "free".
pub fn observe(
    svc: &ChannelService,
    mur_home: &Path,
    channel_id: &str,
    session: &str,
) -> Result<Observed> {
    let lock = match run_lock::try_acquire(svc, channel_id) {
        Ok(l) => l,
        Err(LockDenied::Running(who)) => {
            return Ok(Observed {
                state: SessionState::Running(who),
                last: None,
                lock: None,
            });
        }
        Err(e @ LockDenied::Unavailable(_)) => {
            anyhow::bail!("review session '{session}': {e}")
        }
    };
    let defined = store::fleet_path(mur_home, session).exists();
    let events = svc.load_events(channel_id)?;
    let mut last = events.iter().map(|e| e.ts).max();
    if last.is_none() {
        last = svc
            .store()
            .load_manifest(channel_id)
            .ok()
            .map(|c| c.created_at);
    }
    // The last lifecycle event decides paused/stopped; a later `resumed`
    // (or any round activity) means neither.
    let mut tail: Option<&'static str> = None;
    for ev in &events {
        if let NoteClassification::Review(env) = classify_note_payload(&ev.payload) {
            tail = match env.payload {
                ReviewPayload::SessionStopped { .. } => Some("stopped"),
                ReviewPayload::Paused { .. } => Some("paused"),
                // Notes and rulings do not move the lifecycle.
                ReviewPayload::HumanNote { .. }
                | ReviewPayload::Ruling { .. }
                | ReviewPayload::ModeChanged { .. } => tail,
                _ => None,
            };
        }
    }
    let marker_path = svc
        .store()
        .events_path(channel_id)
        .with_file_name(REVIEW_CORRUPTED_MARKER_FILE);
    let marker = if marker_path.exists() {
        Some(
            std::fs::read_to_string(&marker_path)
                .ok()
                .and_then(|s| serde_json::from_str::<CorruptedMarker>(&s).ok()),
        )
    } else {
        None
    };
    let stopped = tail == Some("stopped");
    let state = match (marker, stopped, defined) {
        (_, true, false) => SessionState::Stopped,
        (Some(Some(m)), _, _) => {
            last = Some(last.map_or(m.detected_at, |l| l.max(m.detected_at)));
            SessionState::Corrupted
        }
        (Some(None), _, _) => SessionState::CorruptedUnreadable(marker_path),
        (None, _, false) => SessionState::Orphaned,
        (None, false, true) if tail == Some("paused") => SessionState::Paused,
        (None, _, true) => SessionState::Crashed,
    };
    Ok(Observed {
        state,
        last,
        lock: Some(lock),
    })
}

/// The ledger for a `session_stopped` written from outside the driver
/// (delete, prune). A channel that no longer folds still gets the event,
/// carrying the highest cumulative figures any readable event recorded, so
/// limits stay monotonic (§8.2).
fn ledger_for_stop(svc: &ChannelService, channel_id: &str) -> Ledger {
    let payloads: Vec<ReviewPayload> = svc
        .load_events(channel_id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some(env.payload),
            _ => None,
        })
        .collect();
    fold_rounds(&payloads).unwrap_or_else(|_| {
        let mut l = Ledger::default();
        for c in payloads.iter().filter_map(cumulative_of) {
            l.exec_time_ms = l.exec_time_ms.max(c.exec_time_ms);
            l.cost_usd_micros = l.cost_usd_micros.max(c.cost_usd_micros);
        }
        l
    })
}

pub(crate) fn cumulative_of(p: &ReviewPayload) -> Option<Cumulative> {
    match p {
        ReviewPayload::Verdict { cumulative, .. }
        | ReviewPayload::Rebuttal { cumulative, .. }
        | ReviewPayload::Paused { cumulative, .. }
        | ReviewPayload::Resumed { cumulative }
        | ReviewPayload::SessionStopped { cumulative, .. }
        | ReviewPayload::ResumedFromCheckpoint { cumulative, .. } => Some(*cumulative),
        _ => None,
    }
}

/// §7.1: end a session from outside its driver. The caller must hold the
/// run lock. Appends `session_stopped` (unless the log already ends
/// stopped), then removes the definition and run state. Crash-safe order:
/// dying after the append leaves an ordinary `stopped` session.
pub fn stop_from_outside(
    svc: &ChannelService,
    mur_home: &Path,
    session: &str,
    channel_id: &str,
    state: &SessionState,
    reason: &str,
    _lock: &DriverLock,
) -> Result<()> {
    if *state != SessionState::Stopped {
        let ledger = ledger_for_stop(svc, channel_id);
        super::session::append_session_stopped(mur_home, channel_id, reason, &ledger)?;
    }
    super::session::remove_session_fleet(mur_home, session)
}

#[cfg(test)]
#[path = "state_tests.rs"]
pub(crate) mod state_tests;
