//! AC2 / §7: `mur fleet review-resume <session>` — pick a paused review
//! session back up at the same round, with the same ledger.
//!
//! A3: the channel is the only state. Resume replays it; nothing else is
//! read. The replay first runs the §8.2 damage check
//! (`load_events_with_damage` + `replay_with_damage`), and only a CLEAN
//! replay resumes here. A damaged log is refused with the damage line and
//! reason (§8.2: "never continue silently from a corrupted or partially
//! rebuilt state"); the §8.2 Continue/Abandon choice is a separate path.
//!
//! The round to continue is the one after the last verdict: a round cut
//! short by the pause left only stateless `turn_sent` events (its rebuttal
//! is signed with its verdict, `loop_driver.rs`), so it restarts from
//! main's turn without double-counting anything.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::fleet::Fleet;

use super::constants::REVIEW_FLEET_PREFIX;
use super::driver::ReviewTransport;
use super::ledger::{Ledger, fold_rounds};
use super::loop_driver::{LoopDriverStop, Members, continue_review_loop};
use super::rollback::{ReplayOutcome, replay_with_damage};
use super::schema::{
    Cumulative, NoteClassification, ReviewPayload, SessionLimits, classify_note_payload,
    to_note_payload,
};
use crate::cmd::fleet::store;

/// Everything needed to continue a paused session.
#[derive(Debug)]
pub struct Resumable {
    pub fleet: Fleet,
    pub ledger: Ledger,
    pub round: u32,
    pub limits: SessionLimits,
    /// Execution time already spent, paused time excluded (AC4).
    pub active: Duration,
}

/// Review payloads in channel order, each with its event timestamp.
fn review_events(events: &[ChannelEvent]) -> Vec<(DateTime<Utc>, ReviewPayload)> {
    events
        .iter()
        .filter(|ev| ev.kind == EventKind::Note)
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some((ev.ts, env.payload)),
            _ => None,
        })
        .collect()
}

/// Execution time from the channel: the sum of every running segment, from
/// `session_started`/`resumed` to the next `paused`. Paused spans never count.
fn active_time(events: &[(DateTime<Utc>, ReviewPayload)]) -> Duration {
    let mut total = chrono::Duration::zero();
    let mut running_since: Option<DateTime<Utc>> = None;
    for (ts, p) in events {
        match p {
            ReviewPayload::SessionStarted { .. }
            | ReviewPayload::Resumed { .. }
            | ReviewPayload::ResumedFromCheckpoint { .. } => {
                running_since.get_or_insert(*ts);
            }
            ReviewPayload::Paused { .. } => {
                if let Some(since) = running_since.take() {
                    total += *ts - since;
                }
            }
            _ => {}
        }
    }
    total.to_std().unwrap_or_default()
}

/// Rebuild a paused session from its channel, refusing anything that is not
/// a clean, paused, still-defined review session.
pub fn prepare_resume(mur_home: &Path, name: &str) -> Result<Resumable> {
    if !name.starts_with(REVIEW_FLEET_PREFIX) {
        bail!("'{name}' is not a review session (names start with '{REVIEW_FLEET_PREFIX}')");
    }
    if !store::fleet_path(mur_home, name).exists() {
        bail!(
            "review session '{name}' has ended or does not exist; only a paused session can be resumed"
        );
    }
    let fleet = store::load_fleet(mur_home, name)?;
    if fleet.members.len() != 2 || fleet.goal.trim().is_empty() {
        bail!("review session '{name}' has no recorded members/task and cannot be resumed");
    }

    let svc = ChannelService::open(mur_home)?;
    let pubkey = crate::channel_verify::actor_pubkey(mur_home, &ChannelActor::System, None)
        .context("cannot read the review writer's key, so the channel cannot be verified")?;
    let (events, report) =
        svc.store()
            .load_events_with_damage(&fleet.channel_id, &pubkey, false)?;
    // The raw text only feeds the lenient cumulative scan; a channel with no
    // events yet has no file, which is an empty log, not an error.
    let raw = match std::fs::read_to_string(svc.store().events_path(&fleet.channel_id)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let raw_lines: Vec<&str> = raw.lines().collect();
    let unverified: HashSet<u64> = report.unverified.iter().map(|e| e.seq).collect();
    match replay_with_damage(
        &events,
        &report.event_lines,
        &report.unparseable_lines,
        &unverified,
        &raw_lines,
    ) {
        ReplayOutcome::Clean(_) => {}
        ReplayOutcome::Partial {
            damage_line,
            damage_reason,
            ..
        }
        | ReplayOutcome::Fatal {
            damage_line,
            damage_reason,
        } => bail!(
            "review session '{name}' cannot be resumed: channel damaged at line {damage_line} \
             ({damage_reason}). Channel: {}",
            svc.store().events_path(&fleet.channel_id).display()
        ),
    }

    let timed = review_events(&events);
    let payloads: Vec<ReviewPayload> = timed.iter().map(|(_, p)| p.clone()).collect();
    if payloads
        .iter()
        .any(|p| matches!(p, ReviewPayload::SessionStopped { .. }))
    {
        bail!("review session '{name}' has already stopped");
    }
    let limits = payloads
        .iter()
        .find_map(|p| match p {
            ReviewPayload::SessionStarted { limits, .. } => Some(*limits),
            _ => None,
        })
        .context("the channel has no session_started event")?;
    let ledger = fold_rounds(&payloads)?;
    if !ledger.paused {
        bail!("review session '{name}' is not paused (is it still running?)");
    }
    let active = active_time(&timed).max(Duration::from_millis(ledger.exec_time_ms));
    Ok(Resumable {
        round: ledger.round + 1,
        fleet,
        ledger,
        limits,
        active,
    })
}

/// Write the signed `resumed` event and continue the loop at the same round.
pub fn resume_session(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    r: Resumable,
    retry_delay: Duration,
) -> Result<(Ledger, LoopDriverStop)> {
    let Resumable {
        fleet,
        mut ledger,
        round,
        limits,
        active,
    } = r;
    let resumed = ReviewPayload::Resumed {
        cumulative: Cumulative {
            exec_time_ms: u64::try_from(active.as_millis()).unwrap_or(u64::MAX),
            cost_usd_micros: ledger.cost_usd_micros,
        },
    };
    ledger.apply(&resumed)?;
    let svc = ChannelService::open(mur_home)?;
    crate::channel_writer::append_as_writer(
        &svc,
        mur_home,
        &fleet.channel_id,
        crate::channel_writer::ROUTER_AGENT,
        ChannelActor::System,
        EventKind::Note,
        to_note_payload(&resumed),
        None,
    )?;
    let members = Members {
        fleet_name: &fleet.name,
        channel_id: &fleet.channel_id,
        main: &fleet.members[0],
        reviewer: &fleet.members[1],
        task: &fleet.goal,
    };
    let run = continue_review_loop(
        transport,
        mur_home,
        &members,
        ledger,
        round,
        limits,
        active,
        retry_delay,
        &Instant::now,
    );
    super::session::end_session(mur_home, &fleet, run)
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod resume_tests;
