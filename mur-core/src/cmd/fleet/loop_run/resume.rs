//! `mur fleet resume <name>` (pause/resume design §5.4, §6, §7, §8.2a).
//!
//! Resume reads the run the `.paused` sentinel names, refuses anything a
//! resume must not continue (a live run, a stop, a commander halt, a missing
//! or foreign checkpoint, a record that is not `Paused`), then re-enters the
//! loop through `run_guarded_from` with the state the checkpoint carried.
//!
//! The checkpoint is written by the run itself, so it is never trusted to
//! widen anything: bounds come from the `run_paused` event signed with the
//! trusted writer's key (`ROUTER_AGENT`, whatever the fleet's `router:`) and
//! are `min`-ed with what `fleet.yaml` allows now, counters are `max`-ed with
//! that event, and the HITL policy is never loosened from `defer`. When no
//! such event verifies (no writer identity, note lost), the checkpoint's
//! bounds are used and resume warns — still `min`-ed with `fleet.yaml`.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use mur_common::channel::{ChannelEvent, EventKind};
use mur_common::hitl::Unanswered;
use mur_common::skill::manifest::ProcedureStep;

use super::checkpoint::{self, Checkpoint};
use super::pause_events::{FleetRunPayload, NoteClassification, RunBounds, classify_note_payload};
use super::{FleetBounds, LoopStop, run_lock};
use crate::cmd::fleet::{control, store};
use crate::executor::delegation::cwd::RunCwd;

/// What the loop re-enters with instead of fresh state (design §5.4 step 5).
#[derive(Debug, Clone)]
pub struct ResumeFrom {
    pub run_id: String,
    /// The iteration to re-enter (0-based); not counted twice.
    pub iteration: u32,
    pub spent_usd: f64,
    pub active_elapsed: Duration,
    pub stuck_elapsed: Duration,
    pub start_seq: u64,
    /// The job the paused iteration claimed; re-used, never re-claimed.
    pub job_id: Option<String>,
    /// The paused iteration's planned steps. Empty when the pause came between
    /// iterations: that iteration is then planned afresh, as nothing ran.
    pub procedure: Vec<ProcedureStep>,
    pub hitl_unanswered: Unanswered,
    /// The bounds the run resolved at its original start — from the signed
    /// `run_paused` when it carries them, else the checkpoint. The loop `min`s
    /// them with the bounds it resolves now (see [`tighten`]).
    pub deadline: Duration,
    pub stuck: mur_common::limits::Stuck,
    pub cost_usd: Option<f64>,
    /// Whether the bounds above came from a signed event. `false` means the
    /// run-written checkpoint was all there was; resume warns.
    pub bounds_authenticated: bool,
    /// The admitted checkpoint's `paused_at`. Re-checked under the run lock:
    /// a newer pause writes a new one, so a stale admission cannot reopen the
    /// run with obsolete counters.
    pub paused_at: String,
}

/// Each bound is the tighter of the original and the current one (design
/// §5.3): a limit lowered during the pause bites, and a raised `fleet.yaml`
/// never widens the original. An edited checkpoint cannot widen it either
/// when the original came from the router-signed `run_paused`
/// ([`ResumeFrom::bounds_authenticated`]); without that event the checkpoint
/// is the original and only `fleet.yaml` caps it. A cap set on either side is
/// never dropped to "unbounded".
pub(super) fn tighten(now: FleetBounds, r: &ResumeFrom) -> FleetBounds {
    use mur_common::limits::Stuck;
    let stuck = match (now.stuck, r.stuck) {
        (Stuck::After(a), Stuck::After(b)) => Stuck::After(a.min(b)),
        (Stuck::After(a), Stuck::Off) | (Stuck::Off, Stuck::After(a)) => Stuck::After(a),
        (Stuck::Off, Stuck::Off) => Stuck::Off,
    };
    let cost_usd = match (now.cost_usd, r.cost_usd) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    FleetBounds {
        deadline: now.deadline.min(r.deadline),
        stuck,
        cost_usd,
        ..now
    }
}

/// The stricter of two unanswered-gate policies: `deny` > `defer` > `wait`.
/// `wait` is the loosest because it lets a gate time out into a denial while
/// nobody may be watching (design §8.2a).
pub(super) fn stricter(a: Unanswered, b: Unanswered) -> Unanswered {
    fn rank(u: Unanswered) -> u8 {
        match u {
            Unanswered::Wait => 0,
            Unanswered::Defer => 1,
            Unanswered::Deny => 2,
        }
    }
    if rank(a) >= rank(b) { a } else { b }
}

/// The unanswered-gate policy a run enforces, and what every pause of it must
/// persist. A fresh run takes `fleet.yaml`'s mode or the process default; a
/// resumed one keeps the policy it was frozen with, tightened by a stricter
/// mode set since — never loosened, across any number of pause cycles.
pub(super) fn effective_hitl(
    fleet_mode: Option<Unanswered>,
    resumed: Option<Unanswered>,
    process_default: Unanswered,
) -> Unanswered {
    match (resumed, fleet_mode) {
        (Some(frozen), Some(now)) => stricter(frozen, now),
        (Some(frozen), None) => frozen,
        (None, Some(now)) => now,
        (None, None) => process_default,
    }
}

fn parse_unanswered(s: &str) -> Result<Unanswered> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|_| anyhow::anyhow!("checkpoint names an unknown HITL policy `{s}`"))
}

/// What the latest signed `run_paused` for a run recorded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SignedPause {
    pub spent_usd: f64,
    pub active_elapsed_ms: u64,
    pub iteration: u32,
    /// The original bounds; `None` on an event written before rev 3.
    pub bounds: Option<RunBounds>,
}

/// The latest signed `run_paused` for `run_id` on the fleet's channel. Only
/// router-signed events count — the checkpoint is what an untrusted run
/// wrote, this is what the trusted writer recorded beside it.
///
/// `verify_event` is not enough: it checks an event against its OWN actor's
/// key, so a member signing a forged pause as itself would pass and its
/// bounds would replace the originals. `is_router_signed` requires a
/// non-agent actor and `ROUTER_AGENT`'s key — the identity `record_pause`
/// writes with for every fleet, including one with a custom `router:`.
fn signed_pause(
    mur_home: &Path,
    channel_id: &str,
    events: &[ChannelEvent],
    run_id: &str,
) -> Option<SignedPause> {
    events
        .iter()
        .rev()
        .filter(|e| e.kind == EventKind::Note && e.sig.is_some())
        .find_map(|e| match classify_note_payload(&e.payload) {
            NoteClassification::FleetRun(env) => match env.payload {
                FleetRunPayload::RunPaused {
                    run_id: id,
                    spent_usd,
                    active_elapsed_ms,
                    iteration,
                    bounds,
                    ..
                } if id == run_id
                    && crate::hitl::authority::is_router_signed(mur_home, channel_id, e) =>
                {
                    Some(SignedPause {
                        spent_usd,
                        active_elapsed_ms,
                        iteration,
                        bounds,
                    })
                }
                _ => None,
            },
            _ => None,
        })
}

/// Build the resume state from a checkpoint and the channel (design §5.3):
/// counters never move down, HITL is never loosened.
pub(super) fn resume_from(
    cp: &Checkpoint,
    signed: Option<SignedPause>,
    unattended_now: bool,
) -> Result<ResumeFrom> {
    let (mut spent, mut active_ms, mut iteration) =
        (cp.spent_usd, cp.active_elapsed_ms, cp.iteration);
    if let Some(s) = signed {
        spent = spent.max(s.spent_usd);
        active_ms = active_ms.max(s.active_elapsed_ms);
        iteration = iteration.max(s.iteration);
    }
    let original = signed.and_then(|s| s.bounds);
    let b = original.unwrap_or(RunBounds {
        deadline_ms: cp.deadline_ms,
        stuck_ms: cp.stuck_ms,
        cost_usd: cp.cost_usd,
    });
    let mut hitl = parse_unanswered(&cp.hitl_unanswered)?;
    // A run that started unattended resolved `defer`; a resume from a terminal
    // would resolve `wait`. Never loosen: the stricter of the frozen policy and
    // what this process would pick on its own.
    if unattended_now {
        hitl = stricter(hitl, Unanswered::Defer);
    }
    Ok(ResumeFrom {
        run_id: cp.run_id.clone(),
        iteration,
        spent_usd: spent,
        active_elapsed: Duration::from_millis(active_ms),
        stuck_elapsed: Duration::from_millis(cp.stuck_elapsed_ms),
        start_seq: cp.start_seq,
        job_id: cp.job_id.clone(),
        // A signed counter ahead of the checkpoint means the checkpoint is not
        // the latest word on this iteration; re-plan rather than replay a
        // procedure for an iteration it does not describe.
        procedure: if iteration == cp.iteration {
            cp.procedure.clone()
        } else {
            vec![]
        },
        hitl_unanswered: hitl,
        deadline: Duration::from_millis(b.deadline_ms),
        stuck: match b.stuck_ms {
            Some(ms) => mur_common::limits::Stuck::After(Duration::from_millis(ms)),
            None => mur_common::limits::Stuck::Off,
        },
        cost_usd: b.cost_usd,
        bounds_authenticated: original.is_some(),
        paused_at: cp.paused_at.clone(),
    })
}

/// Why a fleet cannot be resumed right now, checked in the order of design
/// §5.4 steps 1-4. `Ok` carries the checkpoint to continue from.
pub(super) fn admit(mur_home: &Path, name: &str) -> Result<Checkpoint> {
    if control::paused_run_id(mur_home, name).is_some() && run_lock::is_held(mur_home, name) {
        bail!(
            "a run of '{name}' is live; it is not paused yet — wait for it, or check `mur fleet status {name}`"
        );
    }
    admit_state(mur_home, name)
}

/// Re-run admission with the run lock held, and require that the pause is
/// still the one the caller admitted. Two `resume`s can both pass [`admit`]
/// before either takes the lock; the later one would otherwise reopen the run
/// from a snapshot a newer pause (or a finish) has superseded, rolling back
/// spend and elapsed time.
pub(super) fn revalidate_under_lock(mur_home: &Path, name: &str, r: &ResumeFrom) -> Result<()> {
    let cp = admit_state(mur_home, name)?;
    if cp.run_id != r.run_id || cp.paused_at != r.paused_at {
        bail!(
            "fleet '{name}': the pause this resume admitted is no longer current (another resume took it) — check `mur fleet status {name}`"
        );
    }
    Ok(())
}

/// Every admission check except "is a run live": [`admit`] asks that before
/// taking the lock, [`revalidate_under_lock`] holds the lock itself.
fn admit_state(mur_home: &Path, name: &str) -> Result<Checkpoint> {
    let stopped = control::is_stopped(mur_home, name);
    let Some(run_id) = control::paused_run_id(mur_home, name) else {
        if stopped {
            bail!(stopped_message(name));
        }
        bail!("fleet '{name}' has no paused run; use `mur fleet run`");
    };
    if stopped {
        bail!(stopped_message(name));
    }
    let gov = crate::cmd::commander::governance_state(mur_home, name);
    match gov {
        Ok(g) if g.killed => bail!(
            "fleet '{name}' was halted by a commander directive and cannot be resumed — inspect it: mur commander status"
        ),
        Ok(g) if matches!(g.budget_ceiling, Some(c) if c == 0.0) => bail!(
            "fleet '{name}' has a zero commander budget ceiling and cannot be resumed — inspect it: mur commander status"
        ),
        Ok(_) => {}
        Err(e) => bail!(
            "fleet '{name}': governance could not be read ({e:#}), so it is not resumed (fail-closed)"
        ),
    }
    let cp = match checkpoint::load(mur_home, &run_id) {
        Ok(Some(cp)) => cp,
        Ok(None) => bail!(
            "fleet '{name}': run {run_id} has no checkpoint, so there is nothing to resume from — `mur fleet stop {name}` discards it"
        ),
        Err(e) => bail!("fleet '{name}': {e:#} — `mur fleet stop {name}` discards the paused run"),
    };
    if cp.run_id != run_id || cp.fleet != name {
        bail!(
            "fleet '{name}': the checkpoint of run {run_id} names another run — `mur fleet stop {name}` discards it"
        );
    }
    match crate::run_status::store::load(mur_home, &run_id) {
        Ok(Some(r)) if awaits_resume(r.state, true) => {}
        Ok(Some(r)) => bail!(
            "fleet '{name}': run {run_id} is `{:?}`, not paused — nothing to resume",
            r.state
        ),
        Ok(None) => bail!(
            "fleet '{name}': run record missing for {run_id}; `mur fleet stop {name}` to discard"
        ),
        Err(e) => bail!(
            "fleet '{name}': run record of {run_id} is unreadable ({e:#}); `mur fleet stop {name}` to discard"
        ),
    }
    Ok(cp)
}

fn stopped_message(name: &str) -> String {
    format!(
        "fleet '{name}' is stopped — `mur fleet start` clears the kill-switch but does not revive a paused run"
    )
}

/// `mur fleet resume <name>`: continue the paused run in the foreground.
pub async fn cmd_fleet_resume(mur_home: &Path, name: &str) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?;
    let cp = admit(mur_home, name)?;
    let events = mur_channel::ChannelService::open(mur_home)?
        .load_events(&fleet.channel_id)
        .unwrap_or_default();
    let signed = signed_pause(mur_home, &fleet.channel_id, &events, &cp.run_id);
    let unattended = !std::io::IsTerminal::is_terminal(&std::io::stdin());
    let from = resume_from(&cp, signed, unattended)?;
    if !from.bounds_authenticated {
        println!(
            "  ⚠ no signed run_paused carries this run's original limits; resuming under the checkpoint's copy, tightened by fleet.yaml (check `mur fleet limits {name}`)"
        );
    }
    println!(
        "Resuming fleet '{name}' (run {}) at iteration {}, {} spent, paused by mur {}…",
        from.run_id,
        from.iteration + 1,
        crate::cmd::limits::fmt_dur(from.active_elapsed),
        cp.binary_version
    );
    let cwd = cp.target_cwd.as_ref().map(|p| RunCwd {
        path: Some(p.into()),
        inferred: false,
    });
    let (stop, iteration, spent) =
        super::run_guarded_from(mur_home, name, cp.goal_override.clone(), cwd, Some(from)).await?;
    let billable =
        crate::cmd::fleet::progress::load_view(mur_home, name).and_then(|v| v.progress.billable);
    println!(
        "{}",
        super::loop_end_line(
            name,
            stop,
            iteration,
            &crate::cmd::fleet::progress::fmt_spend(spent, billable)
        )
    );
    if stop == LoopStop::Paused {
        println!("Paused again. Continue with `mur fleet resume {name}`.");
    }
    Ok(())
}

/// The goal and job a re-entered iteration runs. The checkpoint's job is
/// re-used as it stands — still `Running`, never claimed again — so the queue
/// does not hand this iteration a second job (design §5.4). A job that has
/// since left `Running` (cancelled by hand) is not revived: the iteration
/// falls back to the standing goal.
pub(super) fn reentered_goal(
    mur_home: &Path,
    name: &str,
    standing_goal: &str,
    r: &ResumeFrom,
) -> Result<(String, Option<mur_common::fleet::Job>)> {
    let Some(job_id) = r.job_id.as_deref() else {
        return Ok((standing_goal.to_string(), None));
    };
    let job = crate::cmd::fleet::jobs::list_jobs_raw(mur_home, name)?
        .into_iter()
        .find(|j| j.id == job_id && j.status == mur_common::fleet::JobStatus::Running);
    Ok(match job {
        Some(j) => (j.text.clone(), Some(j)),
        None => (standing_goal.to_string(), None),
    })
}

/// The idempotency key of the `run_resumed` that ends one pause.
pub(super) fn resumed_key(r: &ResumeFrom) -> String {
    format!("run_resumed:{}:{}", r.run_id, r.paused_at)
}

/// Write `run_resumed` (design §5.6). Best-effort like `run_paused`: the run
/// record is the state that matters. Keyed by the pause it ends, so a resume
/// repeated after a crash (§5.4 step 6) records it once, and the next pause
/// cycle — a new `paused_at` — gets its own.
pub(super) fn record_resumed(
    svc: &mur_channel::ChannelService,
    mur_home: &Path,
    fleet: &mur_common::fleet::Fleet,
    r: &ResumeFrom,
) {
    let payload = FleetRunPayload::RunResumed {
        run_id: r.run_id.clone(),
        iteration_run_id: format!("{}-{}", r.run_id, r.iteration),
        iteration: r.iteration,
        spent_usd: r.spent_usd,
        active_elapsed_ms: u64::try_from(r.active_elapsed.as_millis()).unwrap_or(u64::MAX),
    }
    .to_note_payload();
    // `System` events verify against `ROUTER_AGENT`, never the fleet's
    // router (#2116) — same writer as `record_pause`.
    if let Err(error) = crate::channel_writer::append_as_writer(
        svc,
        mur_home,
        &fleet.channel_id,
        crate::channel_writer::ROUTER_AGENT,
        mur_common::channel::ChannelActor::System,
        EventKind::Note,
        payload,
        Some(resumed_key(r)),
    ) {
        tracing::warn!(fleet = %fleet.name, %error, "could not write run_resumed to the channel");
    }
}

/// Whether a run in `state` still waits for `mur fleet resume`: `Paused`, or
/// `Running` with its checkpoint still on disk — a resume that crashed after
/// flipping the record and before deleting the checkpoint, which it deletes
/// last (design §5.4 step 6, rev 3 §14 item 3). Only meaningful with no live
/// owner: every caller asks with the run lock held or after `is_held` said no,
/// so a `Running` record here has a dead pid.
pub(super) fn awaits_resume(state: crate::run_status::State, checkpoint_present: bool) -> bool {
    use crate::run_status::State;
    match state {
        State::Paused => true,
        State::Running => checkpoint_present,
        _ => false,
    }
}

/// [`awaits_resume`] for `run_id`, read from its record and checkpoint.
fn pending_resume(mur_home: &Path, run_id: &str) -> bool {
    crate::run_status::store::load(mur_home, run_id)
        .ok()
        .flatten()
        .is_some_and(|r| {
            awaits_resume(
                r.state,
                checkpoint::load(mur_home, run_id).ok().flatten().is_some(),
            )
        })
}

/// A fresh run while a run of this fleet is paused would orphan it and reset
/// its limits (design §8.4). Checked with the run lock held, so it is not
/// check-then-act. An interrupted resume counts as paused. A sentinel is
/// removed as a crash leftover only when the record is not paused AND no
/// checkpoint exists (design §8.5): a checkpoint is a pause nobody discarded,
/// so only `mur fleet stop` may drop it.
pub(super) fn refuse_fresh_run_over_pause(mur_home: &Path, name: &str) -> Result<()> {
    let Some(run_id) = control::paused_run_id(mur_home, name) else {
        return Ok(());
    };
    if pending_resume(mur_home, &run_id) {
        bail!(
            "fleet '{name}' has a paused run ({run_id}): continue it with `mur fleet resume {name}`, or discard it with `mur fleet stop {name}`"
        );
    }
    // Only a checkpoint that is provably absent (`Ok(None)`) makes the
    // sentinel stale. An unreadable one still exists; dropping the sentinel
    // over it would orphan it without the `mur fleet stop` §8.5 requires.
    match checkpoint::load(mur_home, &run_id) {
        Ok(None) => {}
        Ok(Some(_)) => bail!(
            "fleet '{name}' holds a checkpoint for run {run_id}, whose record is not paused, so it cannot be resumed: discard it with `mur fleet stop {name}`"
        ),
        Err(e) => bail!(
            "fleet '{name}': the checkpoint of run {run_id} cannot be read ({e:#}), so the pause is kept: discard it with `mur fleet stop {name}`"
        ),
    }
    let _ = std::fs::remove_file(control::paused_path(mur_home, name));
    Ok(())
}

/// `mur fleet stop` on a paused run (design §6): no loop process is alive to
/// see the kill-switch, so the operator command ends the run itself. Called
/// after `.stopped` is written. Best-effort per piece; returns whether a
/// paused run was ended.
pub(crate) fn end_paused_run(mur_home: &Path, name: &str) -> bool {
    let Some(run_id) = control::paused_run_id(mur_home, name) else {
        return false;
    };
    // A live run still holds the lock: it sees `.stopped` itself and ends
    // `Stopped` on its own path. Touch nothing.
    if run_lock::is_held(mur_home, name) {
        return false;
    }
    let paused = pending_resume(mur_home, &run_id);
    if paused {
        if let Ok(Some(cp)) = checkpoint::load(mur_home, &run_id)
            && let Some(job_id) = cp.job_id
            && let Ok(jobs) = crate::cmd::fleet::jobs::list_jobs_raw(mur_home, name)
            && let Some(mut job) = jobs.into_iter().find(|j| j.id == job_id)
            && job.status == mur_common::fleet::JobStatus::Running
        {
            job.status = mur_common::fleet::JobStatus::Canceled;
            job.finished_at = Some(chrono::Utc::now().to_rfc3339());
            job.error = Some("stopped by `mur fleet stop` while paused".to_string());
            let _ = crate::cmd::fleet::jobs::save_job(mur_home, name, &job);
        }
        // The loop's record AND the paused iteration's: `mur fleet status`
        // shows the iteration, which otherwise stayed `paused` forever.
        let iteration_run_id = checkpoint::load(mur_home, &run_id)
            .ok()
            .flatten()
            .map(|cp| cp.iteration_run_id);
        for id in std::iter::once(run_id.as_str()).chain(iteration_run_id.as_deref()) {
            let _ = crate::run_status::store::update(mur_home, id, |r| {
                r.state = crate::run_status::State::Stopped
            });
        }
        if let Some((mut p, _)) = crate::cmd::fleet::progress::load(mur_home, name)
            && p.run_id == run_id
        {
            p.finished_at = Some(chrono::Utc::now().to_rfc3339());
            p.outcome = Some("stopped".to_string());
            p.save(mur_home, name);
        }
        announce_discard(mur_home, name, &run_id);
    }
    checkpoint::remove(mur_home, &run_id);
    checkpoint::clear_pause_refusal(mur_home, &run_id);
    let _ = std::fs::remove_file(control::paused_path(mur_home, name));
    paused
}

/// The channel's record that the paused run is over: one terminal `canceled`
/// `StateChange` per run id the pause left open — the iteration's (whose
/// `run.json` the rebuild folds) and the loop's, the same pair a run stopped
/// while live would have written. Without it the channel's last word is
/// `run_paused`, and anything rebuilt from the channel reports a discarded
/// run as still going. Best-effort, like every stop announcement.
fn announce_discard(mur_home: &Path, name: &str, run_id: &str) {
    let Ok(fleet) = store::load_fleet(mur_home, name) else {
        return;
    };
    let Ok(svc) = mur_channel::ChannelService::open(mur_home) else {
        return;
    };
    let mut ids = vec![run_id.to_string()];
    if let Ok(Some(cp)) = checkpoint::load(mur_home, run_id) {
        ids.insert(0, cp.iteration_run_id);
    }
    for id in ids {
        let payload = serde_json::json!({
            "from": "working",
            "to": super::terminal_state_for(LoopStop::Stopped),
            "stop_reason": super::outcome_label(LoopStop::Stopped),
            "remedy": super::stop_remedy(LoopStop::Stopped, name),
            "run_id": id,
        });
        // `System` events verify against `ROUTER_AGENT` (#2116).
        if let Err(error) = crate::channel_writer::append_as_writer(
            &svc,
            mur_home,
            &fleet.channel_id,
            crate::channel_writer::ROUTER_AGENT,
            mur_common::channel::ChannelActor::System,
            EventKind::StateChange,
            payload,
            None,
        ) {
            tracing::warn!(fleet = %name, %error, "could not write the discarded run's end to the channel");
        }
    }
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
