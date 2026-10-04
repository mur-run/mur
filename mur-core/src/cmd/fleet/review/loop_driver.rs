//! D3 / AC12: the full two-party review loop — "a full loop runs to
//! `approve`" (spec line 511).
//!
//! Wires together pieces that already exist rather than inventing new ones:
//! each turn goes through `driver::run_turn_with_retry` (so the pre-send
//! kill-switch check (A4) and the transport-failure retry/pause behaviour
//! (§8.1, AC14) stay covered — this module adds no second way to send), the
//! reviewer's reply is parsed into the §3.2 wire shape using the EXISTING
//! schema types (`VerdictKind`, `NewFindingDto`, `PriorUpdateDto` —
//! `schema.rs` is not modified), folded into the ledger via `Ledger::apply`,
//! and every payload is appended to the channel with
//! `ChannelService::append_signed`.
//!
//! Scope: only what AC12's happy path needs. §3.4 rebuttal, §3.3 disputes
//! and escalation, §5 auto mode, and §8.2 replay/resume are out of scope
//! here — see `review/mod.rs`'s work-in-progress note and D1/D2/the other
//! modules in this directory for those.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::limits::Stuck;
use serde::Deserialize;

use super::driver::{RetryOutcome, ReviewTransport, run_turn_with_retry};
use super::ledger::Ledger;
use super::schema::{
    Cumulative, Mode, NewFindingDto, PriorUpdateDto, ReviewPayload, Role, SessionLimits,
    VerdictKind, to_note_payload,
};
use super::wire::{extract_verdict_json, main_turn_params, reviewer_turn_params};
use crate::cmd::fleet::loop_run::{LoopStop, check_guards};

/// The reviewer's wire reply (§3.2): `verdict: approve | revise | blocked`,
/// `findings:` (new findings only — the system assigns IDs, never the
/// model, per `NewFindingDto` having no `id` field at all), `prior:` (one
/// entry per previously-issued finding ID not yet closed).
#[derive(Debug, Clone, Deserialize)]
struct VerdictReply {
    verdict: VerdictKind,
    #[serde(default)]
    findings: Vec<NewFindingDto>,
    #[serde(default)]
    prior: Vec<PriorUpdateDto>,
}

/// Why [`run_review_loop`] stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopDriverStop {
    /// §3.2/§3.5: the reviewer returned `approve`.
    Approve,
    /// §3.2: "Malformed verdict → retry once with a validation hint → still
    /// malformed → treat as `blocked`." This module does not yet implement
    /// the validation-hint retry (out of AC12 scope); an unparseable reply
    /// is treated as `blocked` directly.
    Blocked,
    /// §3.2: the reviewer returned `blocked` itself.
    ReviewerBlocked,
    /// `.stopped` observed before a send (A4) — see `run_turn_with_retry`.
    Stopped,
    /// The transport paused after exhausting its one retry (§8.1, AC14);
    /// the loop does not keep going once the session has reverted to
    /// semi-auto.
    Paused { reason: String },
    /// One of the three existing limits tripped (§3.5), via `check_guards`.
    Guard(LoopStop),
    /// §3.3 round-stuck: the open set (IDs + statuses) was unchanged across
    /// two consecutive rounds (AC9). Runs alongside the duration `stuck`
    /// guard; whichever trips first stops the session (§3.5, Q1).
    RoundStuck,
}

/// Zero cumulative (§4 requires every turn-ending event to carry one; D3
/// does not yet wire real execution-time/cost accounting — that is a
/// separate concern from AC12's own scope, same note as `driver.rs`'s
/// `write_paused_and_revert`).
fn zero_cumulative() -> Cumulative {
    Cumulative {
        exec_time_ms: 0,
        cost_usd_micros: 0,
    }
}

/// Append one review payload to `channel_id` as the router writer — the
/// SAME identity `driver::write_paused_and_revert` signs `paused` /
/// `mode_changed` with. The verifier resolves a `ChannelActor::System`
/// event to the router's key (`channel_verify::actor_key_dir`), so a
/// caller-chosen key here would write events that fail verification and
/// seal replay at the first one. One writer per session channel, resolved
/// in one place (`channel_writer::writer_key`, incl. the sandbox handoff).
fn append(
    svc: &ChannelService,
    mur_home: &Path,
    channel_id: &str,
    payload: &ReviewPayload,
) -> Result<()> {
    crate::channel_writer::append_as_writer(
        svc,
        mur_home,
        channel_id,
        crate::channel_writer::ROUTER_AGENT,
        ChannelActor::System,
        EventKind::Note,
        to_note_payload(payload),
        None,
    )?;
    Ok(())
}

/// Run the two-party loop (§3.1) to completion: main's turn, then the
/// reviewer's turn, each round, until `approve`, `blocked`, a transport
/// stop/pause, or a guard limit. Returns the folded ledger alongside the
/// stop reason.
#[allow(clippy::too_many_arguments)]
pub fn run_review_loop(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    fleet_name: &str,
    channel_id: &str,
    main: &str,
    reviewer: &str,
    task: &str,
    mode: Mode,
    retry_delay: Duration,
    limits: SessionLimits,
    now: &dyn Fn() -> Instant,
) -> Result<(Ledger, LoopDriverStop)> {
    let svc = ChannelService::open(mur_home)?;
    let mut ledger = Ledger::default();
    // §4: the session opens with `session_started` (members, mode, resolved
    // limits). It carries no round, so it never moves a round boundary on
    // replay. The guards below read deadline/stuck back out of `limits`, so
    // the recorded limits are exactly the enforced ones.
    let deadline = limits.deadline();
    let stuck = limits.stuck();
    let started = ReviewPayload::SessionStarted {
        members: [main.to_string(), reviewer.to_string()],
        mode,
        limits,
    };
    ledger.apply(&started)?;
    append(&svc, mur_home, channel_id, &started)?;
    let start = now();
    // Activity = a turn that returned `RetryOutcome::Sent(_)`. Spec §3.5
    // defines stuck as "no agent-authored channel event for the window", so
    // a long turn that DOES come back with a reply is activity, not a stall:
    // a main coding turn can legitimately run past the stuck window. Stuck
    // is therefore checked only once per round, before main's turn. After
    // each send only the deadline is re-checked (a reply that lands past the
    // user's deadline is discarded). Known limit: a send that never returns
    // is not preempted (the transport call is blocking); catching that needs
    // a watchdog around the transport, not a post-hoc duration check.
    let mut last_activity = now();
    let mut round: u32 = 1;

    loop {
        // §3.5 limits: deadline/stuck passed in by the caller (production
        // gets them from `loop_run::fleet_bounds`); checked once per round,
        // same cadence `loop_run`'s own guarded loop uses.
        let elapsed = now().saturating_duration_since(start);
        let stuck_for = now().saturating_duration_since(last_activity);
        if let Some(stop) = check_guards(round - 1, elapsed, deadline, stuck_for, stuck) {
            return Ok((ledger, LoopDriverStop::Guard(stop)));
        }

        // Main's turn: produce or revise (§3.1). The content is free text
        // (§3.1 says nothing about its shape) — only the reviewer's reply is
        // a structured verdict (§3.2).
        // From round 2 on, main must see what to answer (§3.3: "For each
        // open finding the main agent answers accept | reject | partial").
        let main_params = main_turn_params(task, round, &ledger);
        let main_reply = match run_turn_with_retry(
            transport,
            mur_home,
            fleet_name,
            main,
            &main_params,
            channel_id,
            retry_delay,
        )? {
            RetryOutcome::Stopped => return Ok((ledger, LoopDriverStop::Stopped)),
            RetryOutcome::Paused { reason } => {
                return Ok((ledger, LoopDriverStop::Paused { reason }));
            }
            RetryOutcome::Sent(reply) => {
                append_turn_sent(&svc, mur_home, channel_id, round, Role::Main)?;
                if let Some(stop) = check_guards(
                    round,
                    now().saturating_duration_since(start),
                    deadline,
                    Duration::ZERO,
                    Stuck::Off,
                ) {
                    return Ok((ledger, LoopDriverStop::Guard(stop)));
                }
                reply
            }
        };

        // Reviewer's turn, fed main's output.
        let reviewer_params = reviewer_turn_params(task, round, &main_reply, &ledger);
        let reviewer_reply = match run_turn_with_retry(
            transport,
            mur_home,
            fleet_name,
            reviewer,
            &reviewer_params,
            channel_id,
            retry_delay,
        )? {
            RetryOutcome::Stopped => return Ok((ledger, LoopDriverStop::Stopped)),
            RetryOutcome::Paused { reason } => {
                return Ok((ledger, LoopDriverStop::Paused { reason }));
            }
            RetryOutcome::Sent(reply) => {
                append_turn_sent(&svc, mur_home, channel_id, round, Role::Reviewer)?;
                if let Some(stop) = check_guards(
                    round,
                    now().saturating_duration_since(start),
                    deadline,
                    Duration::ZERO,
                    Stuck::Off,
                ) {
                    return Ok((ledger, LoopDriverStop::Guard(stop)));
                }
                last_activity = now();
                reply
            }
        };

        // Model output is untrusted: build the round's payloads, fold them
        // into a scratch ledger first, and only sign them into the channel
        // once the whole round folds cleanly. A reply that cannot be parsed
        // or that names an unissued finding (§8.2 illegal transition) is
        // treated as `blocked` instead of poisoning the channel for replay.
        let staged = extract_verdict_json(&reviewer_reply)
            .and_then(|json| serde_json::from_str::<VerdictReply>(json).ok())
            .and_then(|parsed| {
                let mut scratch = ledger.clone();
                let payloads = stage_round(&mut scratch, round, &parsed)?;
                Some((parsed.verdict, scratch, payloads))
            });
        let Some((verdict, scratch, payloads)) = staged else {
            let payload = ReviewPayload::Verdict {
                round,
                kind: VerdictKind::Blocked,
                cumulative: zero_cumulative(),
            };
            ledger.apply(&payload)?;
            append(&svc, mur_home, channel_id, &payload)?;
            ledger.note_round_complete();
            return Ok((ledger, LoopDriverStop::Blocked));
        };
        for payload in &payloads {
            append(&svc, mur_home, channel_id, payload)?;
        }
        ledger = scratch;
        let parsed_verdict = verdict;

        // The round is fully folded: snapshot its open set (§3.3, AC9).
        // `ledger::fold_rounds` notes the same boundaries on replay, so the
        // in-memory ledger stays byte-comparable to the channel (AC11).
        ledger.note_round_complete();

        match parsed_verdict {
            VerdictKind::Approve => return Ok((ledger, LoopDriverStop::Approve)),
            VerdictKind::Blocked => return Ok((ledger, LoopDriverStop::ReviewerBlocked)),
            VerdictKind::Revise if ledger.round_stuck => {
                return Ok((ledger, LoopDriverStop::RoundStuck));
            }
            VerdictKind::Revise => {}
        }

        round += 1;
    }
}

/// §4 `turn_sent`, written once a send to `to` has actually gone out
/// (`RetryOutcome::Sent`). Written after the send rather than before, so a
/// `.stopped` observed inside `run_turn_with_retry` never leaves a
/// `turn_sent` for a message that was never delivered. Driver-authored, so
/// it is safe to sign directly (unlike the reviewer's untrusted verdict).
fn append_turn_sent(
    svc: &ChannelService,
    mur_home: &Path,
    channel_id: &str,
    round: u32,
    to: Role,
) -> Result<()> {
    let payload = ReviewPayload::TurnSent {
        round,
        to,
        restart_note: None,
    };
    append(svc, mur_home, channel_id, &payload)
}

/// Fold one reviewer reply into `scratch`, returning the payloads in channel
/// order, or `None` if any of them is an illegal transition.
fn stage_round(
    scratch: &mut Ledger,
    round: u32,
    parsed: &VerdictReply,
) -> Option<Vec<ReviewPayload>> {
    let mut out = Vec::new();
    let verdict = ReviewPayload::Verdict {
        round,
        kind: parsed.verdict,
        cumulative: zero_cumulative(),
    };
    scratch.apply(&verdict).ok()?;
    out.push(verdict);
    for f in &parsed.findings {
        let payload = ReviewPayload::FindingIssued {
            round,
            id: scratch.next_finding_id(),
            severity: f.severity,
            issue: f.issue.clone(),
        };
        scratch.apply(&payload).ok()?;
        out.push(payload);
    }
    for p in &parsed.prior {
        let payload = ReviewPayload::FindingStatus {
            round,
            id: p.id.clone(),
            status: p.status,
            reason: p.reason.clone(),
        };
        scratch.apply(&payload).ok()?;
        out.push(payload);
    }
    Some(out)
}
