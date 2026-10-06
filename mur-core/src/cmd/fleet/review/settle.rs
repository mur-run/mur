//! P2-§5.1–§5.3: turning the human's rulings into channel events. The
//! parsing lives in `ruling.rs` (pure); this module writes. Every in-memory
//! change comes from `Ledger::apply` on the payload that is appended, so
//! replay equals the live ledger (AC-P2-10).

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use mur_channel::ChannelService;

use super::constants::{
    REVIEW_PAUSE_REASON_ESCALATION, RULING_DISCARDED_CLOSED_NOTICE,
    RULING_DISCARDED_SESSION_END_NOTICE,
};
use super::driver::ReviewTransport;
use super::ledger::Ledger;
use super::loop_driver::append;
use super::ruling::{PromptLine, RulingInput, classify_ruling_line};
use super::schema::{Cumulative, PauseKind, ReviewPayload};
use crate::cmd::fleet::control;

/// How [`settle_rulings`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulingOutcome {
    /// No ruling is owed any more; the loop continues.
    Settled,
    /// `q` or EOF: the session is paused awaiting a ruling (P2-§5.2).
    LeftPaused,
    /// `/abandon`: the caller ends the session with reason `escalation`.
    Abandoned,
    /// `.stopped` was set when the prompt returned; the input was discarded.
    KillSwitch,
}

/// What settling and applying rulings needs from the running session.
pub struct RulingCtx<'a> {
    pub transport: &'a dyn ReviewTransport,
    pub svc: &'a ChannelService,
    pub mur_home: &'a Path,
    pub fleet_name: &'a str,
    pub channel_id: &'a str,
    /// True at the `review-resume` prompt of a session already paused
    /// (P2-§6, AC-P2-17): leaving writes nothing.
    pub already_paused: bool,
}

/// P2-§5.1 steps 3–5 and §5.2, for every pending escalation in order.
/// Appends `ruling` (folded into `ledger`) or `paused { kind: escalation }`;
/// never `session_stopped` — the caller maps `Abandoned`/`KillSwitch`.
///
/// `unrecorded_wait` is human wait already taken from the transport that no
/// `turn_sent` carries yet. On `q`/EOF it and the prompt's own wait go onto
/// the `paused` event, because no later `turn_sent` will (QA P1).
pub fn settle_rulings(
    ctx: &RulingCtx,
    ledger: &mut Ledger,
    cumulative: Cumulative,
    unrecorded_wait: Duration,
) -> Result<RulingOutcome> {
    while let Some(pending) = ledger.pending_ruling().first().map(|e| (*e).clone()) {
        let line = ctx.transport.ask_ruling(&pending, ledger)?;
        // §5.1 step 4: the kill-switch wins over whatever was typed.
        if control::is_stopped(ctx.mur_home, ctx.fleet_name) {
            return Ok(RulingOutcome::KillSwitch);
        }
        let open = ledger.open_set().into_keys().collect();
        match classify_ruling_line(&line, &open) {
            PromptLine::Rule(r) => write_ruling(ctx, ledger, &r)?,
            PromptLine::Abandon => return Ok(RulingOutcome::Abandoned),
            PromptLine::Leave => {
                if !ctx.already_paused {
                    let waited = unrecorded_wait + ctx.transport.take_human_wait();
                    let paused = ReviewPayload::Paused {
                        kind: PauseKind::Escalation,
                        reason: REVIEW_PAUSE_REASON_ESCALATION.to_string(),
                        cumulative,
                        human_wait_ms: u64::try_from(waited.as_millis()).unwrap_or(u64::MAX),
                    };
                    ledger.apply(&paused)?;
                    append(ctx.svc, ctx.mur_home, ctx.channel_id, &paused)?;
                }
                return Ok(RulingOutcome::LeftPaused);
            }
            PromptLine::Other(hint) => ctx.transport.show(&hint)?,
        }
    }
    Ok(RulingOutcome::Settled)
}

/// Write one ruling and fold it: the same payload goes to the ledger and
/// the channel. The caller resets its activity clock (§5.1).
pub fn write_ruling(ctx: &RulingCtx, ledger: &mut Ledger, r: &RulingInput) -> Result<()> {
    let payload = ReviewPayload::Ruling {
        finding: r.finding.clone(),
        decision: r.decision,
        text: r.text.clone(),
    };
    ledger.apply(&payload)?;
    append(ctx.svc, ctx.mur_home, ctx.channel_id, &payload)
}

/// P2-§5.3 / AC-P2-18: apply rulings held from the reviewer's send prompt,
/// after the round sealed. Still in the open set → written; otherwise a
/// notice naming the status, nothing written. Returns how many were written.
pub fn apply_held_rulings(
    ctx: &RulingCtx,
    ledger: &mut Ledger,
    held: &mut Vec<RulingInput>,
) -> Result<usize> {
    let mut written = 0;
    for r in held.drain(..) {
        match ledger.open_set().contains_key(&r.finding) {
            true => {
                write_ruling(ctx, ledger, &r)?;
                written += 1;
            }
            false => {
                let status = ledger
                    .finding(&r.finding)
                    .and_then(|f| serde_json::to_value(f.status).ok())
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                ctx.transport.show(
                    &RULING_DISCARDED_CLOSED_NOTICE
                        .replace("{id}", &r.finding)
                        .replace("{status}", &status),
                )?;
            }
        }
    }
    Ok(written)
}

/// P2-§5.3 / AC-P2-19: the round sealed with `verdict` (`approve` or
/// `blocked`); held rulings have no turn left to govern.
pub fn discard_held_rulings(
    transport: &dyn ReviewTransport,
    held: &mut Vec<RulingInput>,
    verdict: &str,
) -> Result<()> {
    for r in held.drain(..) {
        transport.show(
            &RULING_DISCARDED_SESSION_END_NOTICE
                .replace("{id}", &r.finding)
                .replace("{verdict}", verdict),
        )?;
    }
    Ok(())
}
