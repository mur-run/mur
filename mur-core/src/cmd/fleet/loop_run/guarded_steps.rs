//! Two self-contained phases of the guarded loop, split out of `guarded.rs`
//! to keep it under the size limit: the per-iteration commander governance
//! check and the terminal bookkeeping after the loop stops.

use std::ops::ControlFlow;

use super::pause::emit_stop_event;
use super::*;
use crate::run_status::heartbeat::Heartbeat;

/// Commander governance for one iteration (highest priority). `Break` carries
/// the stop the loop must take; `Continue` carries the commander's budget
/// ceiling, if any. Fail-closed: a channel read error halts rather than
/// running ungoverned.
pub(super) fn commander_gate(
    mur_home: &Path,
    name: &str,
    svc: &mur_channel::ChannelService,
    channel_id: &str,
    commander_keys: &[[u8; 32]],
) -> ControlFlow<LoopStop, Option<f64>> {
    let events = match svc.load_events(channel_id) {
        Ok(e) => e,
        Err(_) => {
            // Fail-closed: cannot read governance ⇒ halt, but label the
            // audit as a read error, not a confirmed commander kill.
            // Reachable only via a mid-loop FS-level read fault: a missing
            // channel reads as Ok(empty) and corrupt lines are silently
            // skipped (store::load_events filter_map), so only a genuine
            // read failure errors here. The same fault AT ENTRY surfaces via
            // the `?` on the last_seq load in `run_guarded_inner` → run_guarded returns Err →
            // the caller never runs the loop (also fail-closed). Not
            // unit-tested: portable FS-fault injection is brittle (mirrors
            // the daemon's analogous Err arm in fleet_tick::due_fleets).
            emit_governance_audit(mur_home, name, "read_error", "fail_closed", "", "");
            return ControlFlow::Break(LoopStop::CommanderKilled);
        }
    };
    let gov = mur_channel::governance::fold_governance(&events, channel_id, name, commander_keys);
    if gov.killed {
        let nonce = gov.kill_nonce.as_deref().unwrap_or("");
        let csum = directive_content_sha256(&events, nonce, channel_id);
        emit_governance_audit(mur_home, name, "kill", "halted", nonce, &csum);
        return ControlFlow::Break(LoopStop::CommanderKilled);
    }
    // A zero budget ceiling is a budget halt (spec §6), not a kill.
    if matches!(gov.budget_ceiling, Some(c) if c == 0.0) {
        let nonce = gov.budget_nonce.as_deref().unwrap_or("");
        let csum = directive_content_sha256(&events, nonce, channel_id);
        emit_governance_audit(mur_home, name, "budget_ceiling", "capped", nonce, &csum);
        return ControlFlow::Break(LoopStop::Budget);
    }
    ControlFlow::Continue(gov.budget_ceiling)
}

/// Record why the run stopped: progress file, channel stop event, heartbeat,
/// run-status terminal state, and the triage outcome. Every step is
/// best-effort — a stop must never fail because its bookkeeping did.
#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_run(
    mur_home: &Path,
    name: &str,
    fleet: &Fleet,
    svc: &mur_channel::ChannelService,
    progress: &Mutex<RunProgress>,
    loop_beat: Option<Heartbeat>,
    triage_enabled: bool,
    start: &clock::Stopwatch,
    stop: LoopStop,
    iteration: u32,
    spent: f64,
) {
    // Stamp the terminal state onto the progress file — kept as the last-run
    // record (overwritten by the next run). Best-effort.
    let run_id = {
        let mut g = lock_progress(progress);
        // A paused run is not finished: `finished_at` stays null so the panel
        // and the live-run gate do not read it as over, and `paused_at` says
        // when it stopped (design §5.5).
        if stop == LoopStop::Paused {
            g.paused_at = Some(chrono::Utc::now().to_rfc3339());
        } else {
            g.finished_at = Some(chrono::Utc::now().to_rfc3339());
        }
        g.outcome = Some(outcome_label(stop).to_string());
        g.iteration = iteration;
        g.spend_usd = spent;
        g.save(mur_home, name);
        g.run_id.clone()
    };
    // And onto the channel, where the rail, murmur and the Hub are looking.
    // Until this existed the reason lived only in progress.json, and every
    // surface said "finished" for a run that had hit a cap. Signed as the
    // writer like every other event this run wrote; best-effort like the
    // progress file — a stop must never fail because its announcement did.
    emit_stop_event(svc, mur_home, fleet, stop, iteration, spent, &run_id);
    // Stop the beat BEFORE the terminal write so a late tick cannot
    // resurrect `running` (see Heartbeat::stop).
    if let Some(b) = loop_beat {
        b.stop().await;
    }
    let terminal = loop_terminal_state(stop);
    if let Err(error) = crate::run_status::store::update(mur_home, &run_id, |r| r.state = terminal)
    {
        tracing::warn!(run_id = %run_id, %error, "fleet loop: terminal state not recorded");
    }

    // ── Close the triage loop ──────────────────────────────────────────────
    // The loop path carries the best evidence in the codebase: a real
    // cumulative `spent` and a stop reason the guards themselves produced.
    // `run_id` matches the gate call above, which is what pairs the halves.
    // A pause is not an outcome: the run continues on resume, and scoring it
    // now would pair the verdict against a half-finished run.
    if triage_enabled && stop != LoopStop::Paused {
        crate::executor::triage_gate::record_outcome(
            mur_home,
            &run_id,
            Some(spent),
            start.elapsed(),
            calibration_stop_reason(stop).map(str::to_string),
            matches!(stop, LoopStop::Converged | LoopStop::QueueDrained),
        );
    }
}
