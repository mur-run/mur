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
//! The round to continue is the one after the last SEALED round (§3.3.1):
//! `fold_rounds` drops an unsealed trailing round, so it restarts from
//! main's turn without double-counting anything. That is also what makes a
//! crashed session (§7.0: run lock free, no `paused`) safe to resume.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::fleet::Fleet;

use super::constants::{REVIEW_FLEET_PREFIX, REVIEW_PAUSE_REASON_CRASHED};
use super::driver::ReviewTransport;
use super::ledger::{Ledger, fold_rounds};
use super::loop_driver::{LoopDriverStop, Members, continue_review_loop};
use super::rollback::{ReplayOutcome, replay_with_damage};
use super::run_lock::DriverLock;
use super::schema::{
    Cumulative, NoteClassification, PauseKind, ReviewPayload, SessionLimits, classify_note_payload,
};
use super::settle::{RulingCtx, RulingOutcome, settle_rulings};
use super::state::{SessionState, observe};
use crate::cmd::fleet::store;

/// Everything needed to continue a paused or crashed session. Holds the run
/// lock (§7.0) from the check until the resumed driver ends.
#[derive(Debug)]
pub struct Resumable {
    pub fleet: Fleet,
    /// §7.0: the driver died without pausing. `prepare_resume` has already
    /// recorded `paused` (reason `crashed`) under the lock, before any
    /// prompt (P2-§6, D2); the flag only picks the banner.
    pub crashed: bool,
    /// P2-§6 row 2: the last review event was a `ruling` with no later
    /// `resumed`, so the continue prompt says the ruling is recorded.
    pub ruling_recorded: bool,
    pub lock: DriverLock,
    pub ledger: Ledger,
    pub round: u32,
    pub limits: SessionLimits,
    /// Execution time already spent, paused time excluded (AC4).
    pub active: Duration,
    /// P3b-§8.7 / D10: the round being resumed already had a `turn_sent`
    /// that no verdict sealed, so `fold_rounds` dropped it and main's turn
    /// is sent again (at-least-once; no partial-round persistence).
    pub restarts_round: bool,
}

/// True when `payloads` hold a `turn_sent` for the round after the last
/// sealed one (`sealed_round` is `ledger.round`).
pub(super) fn restarts_round(payloads: &[ReviewPayload], sealed_round: u32) -> bool {
    payloads
        .iter()
        .any(|p| matches!(p, ReviewPayload::TurnSent { round, .. } if *round == sealed_round + 1))
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
/// `session_started`/`resumed` to the next `paused`, less the human-input
/// wait each `turn_sent` or `paused` recorded (§3.5). Paused spans never
/// count. A `paused{crashed}` is written at resume time, long after the
/// crash, so its segment ends at the last event before it instead (QA P1).
fn active_time(events: &[(DateTime<Utc>, ReviewPayload)]) -> Duration {
    let mut total = chrono::Duration::zero();
    let mut running_since: Option<DateTime<Utc>> = None;
    let mut human_wait = Duration::ZERO;
    let mut prev_ts: Option<DateTime<Utc>> = None;
    for (ts, p) in events {
        match p {
            ReviewPayload::TurnSent { human_wait_ms, .. } if running_since.is_some() => {
                human_wait += Duration::from_millis(*human_wait_ms);
            }
            ReviewPayload::Paused {
                reason,
                human_wait_ms,
                ..
            } => {
                if let Some(since) = running_since.take() {
                    let end = match prev_ts {
                        Some(prev) if reason == REVIEW_PAUSE_REASON_CRASHED => prev,
                        _ => *ts,
                    };
                    total += end.max(since) - since;
                    human_wait += Duration::from_millis(*human_wait_ms);
                }
            }
            ReviewPayload::SessionStarted { .. }
            | ReviewPayload::Resumed { .. }
            | ReviewPayload::ResumedFromCheckpoint { .. } => {
                running_since.get_or_insert(*ts);
            }
            _ => {}
        }
        prev_ts = Some(*ts);
    }
    // §7.0 crashed: the last segment never got its `paused`. It counts up to
    // the last readable event; the gap after it held no work.
    if let (Some(since), Some((last, _))) = (running_since, events.last()) {
        total += *last - since;
    }
    total
        .to_std()
        .unwrap_or_default()
        .saturating_sub(human_wait)
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
    // §7.0: liveness is the run lock alone, taken here and held through the
    // resumed run so no second driver can start on this session.
    let observed = observe(&svc, mur_home, &fleet.channel_id, name)?;
    if let SessionState::Running(who) = &observed.state {
        bail!(
            "review session '{name}' is running{}; it cannot be resumed until it pauses or stops",
            who.as_deref()
                .map(|w| format!(" ({w})"))
                .unwrap_or_default()
        );
    }
    let state = observed.state;
    let lock = observed
        .lock
        .context("observe holds the lock for every non-running state")?;
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
            "Channel damaged at line {damage_line}: {damage_reason}. Review session '{name}' \
             cannot be resumed yet (Continue/Abandon is not built).\n\
             Remove it with: mur fleet delete {name}\n\
             Then start a new session with: mur fleet review ...\n\
             Channel: {}",
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
    let crashed = match state {
        SessionState::Paused => false,
        // Crashed needs §3.3.1 sealing: the fold above already dropped the
        // unsealed trailing round, so it is re-run from main's turn.
        SessionState::Crashed => true,
        other => bail!("review session '{name}' cannot be resumed (state: {other:?})"),
    };
    let active = active_time(&timed).max(Duration::from_millis(ledger.exec_time_ms));
    let ruling_recorded = matches!(payloads.last(), Some(ReviewPayload::Ruling { .. }));
    let mut ledger = ledger;
    if crashed {
        // P2-§6 / D2: written now, under the lock and before any prompt, so
        // the running segment ends here and the human's wait at the prompt
        // is never counted as execution time. From here on a crashed
        // session is an already-paused one.
        let paused = ReviewPayload::Paused {
            kind: PauseKind::Other,
            reason: REVIEW_PAUSE_REASON_CRASHED.to_string(),
            cumulative: cumulative_at(&ledger, active),
            human_wait_ms: 0,
        };
        ledger.apply(&paused)?;
        super::loop_driver::append(&svc, mur_home, &fleet.channel_id, &paused)?;
    }
    let restarts = restarts_round(&payloads, ledger.round);
    Ok(Resumable {
        round: ledger.round + 1,
        fleet,
        crashed,
        ruling_recorded,
        lock,
        ledger,
        limits,
        active,
        restarts_round: restarts,
    })
}

fn cumulative_at(ledger: &Ledger, active: Duration) -> Cumulative {
    Cumulative {
        exec_time_ms: u64::try_from(active.as_millis()).unwrap_or(u64::MAX),
        cost_usd_micros: ledger.cost_usd_micros,
    }
}

/// How [`settle_then_resume`] ended.
#[derive(Debug)]
pub enum ResumeEnd {
    /// `q` or EOF at the ruling prompt: nothing written, lock released.
    LeftPaused,
    /// The session ran (or was ended at the prompt) and is now past it.
    Ran(Box<Ledger>, LoopDriverStop),
}

/// P2-§6 row 1: a session that owes a ruling resumes AT the ruling prompt,
/// paused or crashed alike (the crashed `paused` is already written). With
/// nothing owed this is [`resume_session`].
pub fn settle_then_resume(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    mut r: Resumable,
    retry_delay: Duration,
) -> Result<ResumeEnd> {
    if r.ledger.pending_ruling().is_empty() {
        let (ledger, stop) = resume_session(transport, mur_home, r, retry_delay)?;
        return Ok(ResumeEnd::Ran(Box::new(ledger), stop));
    }
    let svc = ChannelService::open(mur_home)?;
    let ctx = RulingCtx {
        transport,
        svc: &svc,
        mur_home,
        fleet_name: &r.fleet.name,
        channel_id: &r.fleet.channel_id,
        already_paused: true,
    };
    let cumulative = cumulative_at(&r.ledger, r.active);
    let outcome = settle_rulings(&ctx, &mut r.ledger, cumulative, Duration::ZERO)?;
    // The wait at this prompt fell between `paused` and `resumed`, which
    // never counts: drop it so the resumed loop does not subtract it again.
    let _ = transport.take_human_wait();
    let stop = match outcome {
        RulingOutcome::Settled => {
            let (ledger, stop) = resume_session(transport, mur_home, r, retry_delay)?;
            return Ok(ResumeEnd::Ran(Box::new(ledger), stop));
        }
        RulingOutcome::LeftPaused => return Ok(ResumeEnd::LeftPaused),
        RulingOutcome::Abandoned => LoopDriverStop::Escalation,
        RulingOutcome::KillSwitch => LoopDriverStop::Stopped,
    };
    let Resumable { fleet, ledger, .. } = r;
    let (ledger, stop) = super::session::end_session(mur_home, &fleet, Ok((ledger, stop)))?;
    Ok(ResumeEnd::Ran(Box::new(ledger), stop))
}

/// Write the signed `resumed` event and continue the loop at the same round.
/// A crashed session's `paused` was already written by [`prepare_resume`].
pub fn resume_session(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    r: Resumable,
    retry_delay: Duration,
) -> Result<(Ledger, LoopDriverStop)> {
    let Resumable {
        fleet,
        lock: _lock,
        mut ledger,
        round,
        limits,
        active,
        ..
    } = r;
    let resumed = ReviewPayload::Resumed {
        cumulative: cumulative_at(&ledger, active),
    };
    let svc = ChannelService::open(mur_home)?;
    ledger.apply(&resumed)?;
    super::loop_driver::append(&svc, mur_home, &fleet.channel_id, &resumed)?;
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
    let run = super::session::apply_requested_pause(transport, mur_home, &fleet.channel_id, run);
    super::session::end_session(mur_home, &fleet, run)
}

/// One line of bare `/review`'s list (spec §3.4).
#[allow(dead_code)] // wired in PR 3 (Task 7): bare `/review` lists these
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PausedRow {
    pub name: String,
    pub state: SessionState,
    pub last: Option<DateTime<Utc>>,
}

/// Review sessions MURMUR can show in bare `/review`: paused and crashed
/// (offered for resume) and running in another process (listed, not offered).
#[allow(dead_code)] // wired in PR 3 (Task 7): bare `/review` calls it
pub fn list_paused(mur_home: &Path) -> Result<Vec<PausedRow>> {
    let svc = ChannelService::open(mur_home)?;
    let mut ids = svc.store().list_ids()?;
    ids.sort();
    let mut rows = Vec::new();
    for id in &ids {
        let Some(session) = super::prune::review_session_of(id) else {
            continue;
        };
        // `observe` takes the run lock for every non-running state; the
        // `Observed` is dropped each iteration, so listing holds nothing.
        let observed = observe(&svc, mur_home, id, session)?;
        if matches!(
            observed.state,
            SessionState::Paused | SessionState::Crashed | SessionState::Running(_)
        ) {
            rows.push(PausedRow {
                name: session.to_string(),
                state: observed.state,
                last: observed.last,
            });
        }
    }
    Ok(rows)
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod resume_tests;

#[cfg(test)]
#[path = "resume_ruling_tests.rs"]
mod resume_ruling_tests;

#[cfg(test)]
#[path = "paused_list_tests.rs"]
mod paused_list_tests;
