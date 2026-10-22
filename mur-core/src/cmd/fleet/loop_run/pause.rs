//! The pause path of the fleet loop (pause/resume design §5.2, §5.3): build
//! the checkpoint from what the loop knows at the moment it halts, write it,
//! then record `run_paused` on the channel.
//!
//! Order matters: the checkpoint is written FIRST, so a `run_paused` note never
//! claims a pause that has nothing to resume from.

use super::checkpoint::{self, CHECKPOINT_SCHEMA, Checkpoint};
use super::pause_events::{FleetRunPayload, PauseReason};
use super::*;
use mur_common::skill::manifest::ProcedureStep;

/// Everything the checkpoint is built from. Borrowed from the loop's locals so
/// the loop does not have to assemble a [`Checkpoint`] inline.
pub(super) struct PauseSnapshot<'a> {
    pub mur_home: &'a Path,
    pub fleet: &'a Fleet,
    /// The outer run id.
    pub run_id: &'a str,
    /// The iteration to re-enter (0-based).
    pub iteration: u32,
    pub spent_usd: f64,
    pub active_elapsed: Duration,
    pub stuck_elapsed: Duration,
    pub start_seq: u64,
    pub job_id: Option<&'a str>,
    pub goal_override: Option<&'a str>,
    pub bounds: &'a FleetBounds,
    pub target_cwd: Option<&'a str>,
    /// The paused iteration's planned steps; empty for a pause taken between
    /// iterations, which resume plans afresh. (Every fleet procedure is built
    /// with no `variables`, so the steps are the whole procedure.)
    pub procedure: &'a [ProcedureStep],
    /// The policy the run is enforcing — persisted as is, never re-derived
    /// from `fleet.yaml` and the TTY, which a terminal resume would loosen.
    pub hitl: mur_common::hitl::Unanswered,
}

/// Write the checkpoint, then the `run_paused` note. An `Err` means NO
/// checkpoint exists and the caller must not report the run paused.
pub(super) fn record_pause(svc: &mur_channel::ChannelService, s: &PauseSnapshot<'_>) -> Result<()> {
    let iteration_run_id = format!("{}-{}", s.run_id, s.iteration);
    let hitl_unanswered = serde_json::to_value(s.hitl)?
        .as_str()
        .unwrap_or("defer")
        .to_string();
    let millis = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    let cp = Checkpoint {
        schema: CHECKPOINT_SCHEMA,
        run_id: s.run_id.to_string(),
        iteration_run_id: iteration_run_id.clone(),
        fleet: s.fleet.name.clone(),
        iteration: s.iteration,
        spent_usd: s.spent_usd,
        active_elapsed_ms: millis(s.active_elapsed),
        stuck_elapsed_ms: millis(s.stuck_elapsed),
        start_seq: s.start_seq,
        job_id: s.job_id.map(str::to_string),
        goal_override: s.goal_override.map(str::to_string),
        deadline_ms: millis(s.bounds.deadline),
        stuck_ms: match s.bounds.stuck {
            mur_common::limits::Stuck::Off => None,
            mur_common::limits::Stuck::After(d) => Some(millis(d)),
        },
        cost_usd: s.bounds.cost_usd,
        hitl_unanswered,
        target_cwd: s.target_cwd.map(str::to_string),
        procedure: s.procedure.to_vec(),
        paused_at: chrono::Utc::now().to_rfc3339(),
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    checkpoint::save(s.mur_home, &cp)?;

    let payload = FleetRunPayload::RunPaused {
        run_id: s.run_id.to_string(),
        iteration_run_id,
        iteration: s.iteration,
        spent_usd: s.spent_usd,
        active_elapsed_ms: cp.active_elapsed_ms,
        reason: PauseReason::Operator,
    }
    .to_note_payload();
    // Best-effort: the checkpoint is the record that matters, and resume
    // works from it alone if the note is missing.
    if let Err(error) = crate::channel_writer::append_as_writer(
        svc,
        s.mur_home,
        &s.fleet.channel_id,
        s.fleet.router_or_concierge(),
        ChannelActor::System,
        mur_common::channel::EventKind::Note,
        payload,
        None,
    ) {
        tracing::warn!(fleet = %s.fleet.name, %error, "could not write run_paused to the channel");
    }
    Ok(())
}

/// One System `state-change` carrying why the loop stopped and the way out.
/// `from` is always `working`: a loop that is ending was running.
pub(super) fn emit_stop_event(
    svc: &mur_channel::ChannelService,
    mur_home: &Path,
    fleet: &Fleet,
    stop: LoopStop,
    iterations: u32,
    spent_usd: f64,
    run_id: &str,
) {
    // A pause is not an ending: no terminal StateChange, or the first
    // reconcile would cancel the job that is meant to resume (design §5.2).
    // The `run_paused` note is written by the pause path instead.
    if stop == LoopStop::Paused {
        return;
    }
    let payload = serde_json::json!({
        "from": "working",
        "to": terminal_state_for(stop),
        "stop_reason": outcome_label(stop),
        "remedy": stop_remedy(stop, &fleet.name),
        "iterations": iterations,
        "spent_usd": spent_usd,
        "run_id": run_id,
    });
    if let Err(e) = crate::channel_writer::append_as_writer(
        svc,
        mur_home,
        &fleet.channel_id,
        fleet.router_or_concierge(),
        ChannelActor::System,
        mur_common::channel::EventKind::StateChange,
        payload,
        None,
    ) {
        tracing::warn!(fleet = %fleet.name, error = %e, "could not write the stop reason to the channel");
    }
}

/// Tell `mur fleet pause` this run cannot be paused, and why. Logged as well,
/// since nobody may be watching the checkpoint directory.
pub(super) fn refuse_pause(mur_home: &Path, run_id: &str, reason: &dyn std::fmt::Display) {
    let reason = reason.to_string();
    tracing::warn!(run_id, %reason, "pause refused; the run keeps going");
    println!("  ⚠ pause refused — the run keeps going: {reason}");
    checkpoint::refuse_pause(mur_home, run_id, &reason);
}
