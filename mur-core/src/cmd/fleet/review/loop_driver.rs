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
//! Both structured replies (the reviewer's verdict, main's rebuttal once
//! findings are open) are validated by `verdict.rs` and retried once with a
//! validation hint (§3.2/§3.4, AC5). §5 auto mode and the reviewer's
//! withdraw/insist answer to a reject are out of scope here — see
//! `review/mod.rs`'s work-in-progress note.

use std::path::Path;
use std::time::{Duration, Instant};

use super::constants::{
    MALFORMED_RESPONSE_RETRIES, REVIEW_PAUSE_REASON_ESCALATION, REVIEW_VALIDATION_HINT,
    RULING_REGENERATED_BANNER, RULING_SESSION_END_APPROVE, RULING_SESSION_END_BLOCKED,
};
use super::driver::{FlushFailed, RetryOutcome, ReviewTransport, SendGate, run_turn_with_retry};
use super::ledger::Ledger;
use super::ruling::RulingInput;
use super::schema::{
    Cumulative, HumanNote, Mode, ReviewPayload, Role, SessionLimits, VerdictKind, to_note_payload,
};
use super::settle::{
    RulingCtx, RulingOutcome, apply_held_rulings, discard_held_rulings, settle_rulings,
    write_ruling,
};
use super::verdict::{parse_rebuttal, parse_verdict, zero_cumulative};
use super::wire::{main_turn_params, message_text, reviewer_turn_params, text_message_params};
use crate::cmd::fleet::control;
use crate::cmd::fleet::loop_run::{LoopStop, check_guards};
use anyhow::Result;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::limits::Stuck;

/// Why [`run_review_loop`] stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopDriverStop {
    /// §3.2/§3.5: the reviewer returned `approve`.
    Approve,
    /// §3.2 / §3.4 (AC5): `role`'s reply was malformed, was re-sent once
    /// with a validation hint, and was malformed again.
    Blocked { role: Role },
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
    /// A member's task ended `failed`/`cancelled` (e.g. a tool call the
    /// HITL gate denied). Reported as itself, never as a malformed verdict:
    /// the reply was not bad, there was no reply.
    TaskFailed { member: String, cause: String },
    /// §3.3 round-stuck: the open set (IDs + statuses) was unchanged across
    /// two consecutive rounds (AC9). Runs alongside the duration `stuck`
    /// guard; whichever trips first stops the session (§3.5, Q1).
    RoundStuck,
    /// The human typed `/abandon` at the ruling prompt (P2-§5.1). An
    /// escalation itself no longer stops the loop: it waits for a ruling.
    Escalation,
}

/// Append one review payload to `channel_id` as the router writer — the
/// SAME identity `driver::write_paused_and_revert` signs `paused` /
/// `mode_changed` with. The verifier resolves a `ChannelActor::System`
/// event to the router's key (`channel_verify::actor_key_dir`), so a
/// caller-chosen key here would write events that fail verification and
/// seal replay at the first one. One writer per session channel, resolved
/// in one place (`channel_writer::writer_key`, incl. the sandbox handoff).
pub(super) fn append(
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
    let started = ReviewPayload::SessionStarted {
        members: [main.to_string(), reviewer.to_string()],
        mode,
        limits,
    };
    ledger.apply(&started)?;
    append(&svc, mur_home, channel_id, &started)?;
    let members = Members {
        fleet_name,
        channel_id,
        main,
        reviewer,
        task,
    };
    continue_review_loop(
        transport,
        mur_home,
        &members,
        ledger,
        1,
        limits,
        Duration::ZERO,
        retry_delay,
        now,
    )
}

/// Who and what a session is about — the fixed part of every turn.
pub struct Members<'a> {
    pub fleet_name: &'a str,
    pub channel_id: &'a str,
    pub main: &'a str,
    pub reviewer: &'a str,
    pub task: &'a str,
}

/// Drive the loop from `round` on over an already-folded `ledger` (a fresh
/// session, or one rebuilt by replay on resume, AC2). `active_before` is
/// the execution time already spent; the deadline counts only that plus
/// time from now, never time spent paused (AC4).
#[allow(clippy::too_many_arguments)]
pub fn continue_review_loop(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    m: &Members<'_>,
    ledger: Ledger,
    round: u32,
    limits: SessionLimits,
    active_before: Duration,
    retry_delay: Duration,
    now: &dyn Fn() -> Instant,
) -> Result<(Ledger, LoopDriverStop)> {
    let start = now();
    let mut run = LoopRun {
        transport,
        svc: ChannelService::open(mur_home)?,
        mur_home,
        fleet_name: m.fleet_name,
        channel_id: m.channel_id,
        main: m.main,
        reviewer: m.reviewer,
        task: m.task,
        retry_delay,
        deadline: limits.deadline(),
        stuck: limits.stuck(),
        now,
        start,
        active_before,
        human_wait: Duration::ZERO,
        round_sent: Vec::new(),
        held: Vec::new(),
        pending: Vec::new(),
        carried_wait: Duration::ZERO,
        last_activity: start,
    };
    run.drive(ledger, round)
}

/// One running loop: everything a turn needs, so the per-turn helper does
/// not take a dozen arguments.
struct LoopRun<'a> {
    transport: &'a dyn ReviewTransport,
    svc: ChannelService,
    mur_home: &'a Path,
    fleet_name: &'a str,
    channel_id: &'a str,
    main: &'a str,
    reviewer: &'a str,
    task: &'a str,
    retry_delay: Duration,
    deadline: Duration,
    stuck: Stuck,
    now: &'a dyn Fn() -> Instant,
    start: Instant,
    /// Execution time spent before this run (a resumed session).
    active_before: Duration,
    /// §3.5 human-input wait in this run so far (send and tool-approval
    /// prompts). Not execution time, so [`LoopRun::elapsed`] subtracts it.
    /// Distinct from a §7.0 pause: the driver is alive, blocked on a human.
    human_wait: Duration,
    /// `turn_sent` events signed in the current round, not yet folded. They
    /// fold into the round ledger only when the round seals, as
    /// `ledger::fold_rounds` does on replay (AC-P2-10): a round cut short
    /// never counts a binding-note delivery.
    round_sent: Vec<ReviewPayload>,
    /// Rulings typed at the reviewer's send prompt this round, applied
    /// after the seal (P2-§5.3, AC-P2-18/19).
    held: Vec<RulingInput>,
    /// P3a-§5: notes typed at the current turn's send prompt, not yet on
    /// the channel. Kept across a boundary `/rule` (the rebuilt message
    /// carries them); dropped when the turn stops before consent.
    pending: Vec<HumanNote>,
    /// Human wait at the ruling prompt, not yet recorded on a `turn_sent`:
    /// the next send carries it in `human_wait_ms` (P2-§5.1, AC-P2-7).
    carried_wait: Duration,
    // Activity = a turn that returned `RetryOutcome::Sent(_)`. Spec §3.5
    // defines stuck as "no agent-authored channel event for the window", so
    // a long turn that DOES come back with a reply is activity, not a stall:
    // a main coding turn can legitimately run past the stuck window. Stuck
    // is therefore checked only once per round, before main's turn. After
    // each send only the deadline is re-checked (a reply that lands past the
    // user's deadline is discarded). Known limit: a send that never returns
    // is not preempted (the transport call is blocking); catching that needs
    // a watchdog around the transport, not a post-hoc duration check.
    last_activity: Instant,
}

/// The send-gate inputs a turn carries: the open set a `/rule` is checked
/// against, and whether the human already consented (main's message rebuilt
/// after a boundary ruling, P2-§5.3).
struct TurnGate<'a> {
    open: &'a std::collections::BTreeSet<String>,
    pre_confirmed: bool,
}

/// The ledgers a turn reads and a note flush writes (the two-ledger rule).
/// `primary` is what the message generator and the validator read; `also`
/// is the sealed ledger on the reviewer's side, so a session that stops
/// mid-reviewer-turn returns a ledger replay can reproduce.
struct Side<'l> {
    primary: &'l mut Ledger,
    also: Option<&'l mut Ledger>,
}

/// How one validated turn ended.
enum Turn<T> {
    /// The reply validated; `value` is what the validator produced.
    Accepted { reply: String, value: T },
    /// The loop must stop here.
    Stop(LoopDriverStop),
    /// `/rule` at main's send prompt: nothing sent (P2-§5.3).
    RuleFirst(RulingInput),
}

impl LoopRun<'_> {
    fn elapsed(&self) -> Duration {
        let wall = (self.now)().saturating_duration_since(self.start);
        self.active_before + wall.saturating_sub(self.human_wait)
    }

    /// The round loop, from `round` on, over an already-folded `ledger`.
    fn drive(&mut self, mut ledger: Ledger, mut round: u32) -> Result<(Ledger, LoopDriverStop)> {
        loop {
            // §3.5 limits, checked once per round — same cadence
            // `loop_run`'s own guarded loop uses.
            self.round_sent.clear();
            let stuck_for = (self.now)().saturating_duration_since(self.last_activity);
            if let Some(stop) = check_guards(
                round - 1,
                self.elapsed(),
                self.deadline,
                stuck_for,
                self.stuck,
            ) {
                return Ok((ledger, LoopDriverStop::Guard(stop)));
            }

            // Main's turn: produce or revise (§3.1). With findings open it
            // must also answer each one (§3.4), machine-validated like the
            // verdict; with none open its reply is free text.
            let task = self.task;
            let mut pre_confirmed = false;
            let (main_reply, rebuttal) = loop {
                let main_open = open_ids(&ledger);
                let open = !main_open.is_empty();
                let turn = self.turn(
                    Role::Main,
                    round,
                    Side {
                        primary: &mut ledger,
                        also: None,
                    },
                    |l, notes| main_turn_params(task, round, l, notes),
                    TurnGate {
                        open: &main_open,
                        pre_confirmed,
                    },
                    |l, reply| {
                        if open {
                            parse_rebuttal(l, round, reply).map(Some)
                        } else {
                            Ok(None)
                        }
                    },
                )?;
                match turn {
                    Turn::Stop(stop) => return Ok((ledger, stop)),
                    Turn::Accepted { reply, value } => break (reply, value),
                    Turn::RuleFirst(r) => {
                        // P2-§5.3: main's prompt is the round boundary. The
                        // kill-switch first; then write, rebuild from the new
                        // ledger (deterministic: a template, no model call),
                        // show it in full, and send without asking again.
                        if control::is_stopped(self.mur_home, self.fleet_name) {
                            return Ok((ledger, LoopDriverStop::Stopped));
                        }
                        write_ruling(&self.ruling_ctx(false), &mut ledger, &r)?;
                        self.last_activity = (self.now)();
                        // P3a-§5.2: the rebuilt message carries the pending
                        // notes; the send that follows flushes them.
                        let params = main_turn_params(task, round, &ledger, &self.pending);
                        let text = message_text(&params).unwrap_or_default();
                        self.transport
                            .show(&format!("{RULING_REGENERATED_BANNER}\n{text}"))?;
                        pre_confirmed = true;
                    }
                }
            };
            // The rebuttal is signed together with the verdict at round end,
            // never on its own: a round cut short (pause, stop, deadline)
            // then leaves only stateless `turn_sent` events, so resuming it
            // from main's turn (AC2) cannot count a reject twice.
            let mut round_ledger = ledger.clone();
            self.fold_round_sent(&mut round_ledger)?;
            if let Some(r) = &rebuttal {
                round_ledger.apply(r)?;
            }

            // Reviewer's turn, fed main's output. Model output is untrusted:
            // the verdict is folded into a scratch ledger first and signed
            // only once the whole round folds cleanly, so a bad reply never
            // poisons the channel for replay.
            let reviewer_open = open_ids(&round_ledger);
            let staged = match self.turn(
                Role::Reviewer,
                round,
                Side {
                    primary: &mut round_ledger,
                    also: Some(&mut ledger),
                },
                |l, notes| reviewer_turn_params(task, round, &main_reply, l, notes),
                TurnGate {
                    open: &reviewer_open,
                    pre_confirmed: false,
                },
                |l, reply| parse_verdict(l, round, reply),
            )? {
                Turn::Stop(LoopDriverStop::Blocked { role }) => {
                    discard_held_rulings(
                        self.transport,
                        &mut self.held,
                        RULING_SESSION_END_BLOCKED,
                    )?;
                    // §3.2: still malformed → treat as `blocked`.
                    if let Some(r) = &rebuttal {
                        self.append(r)?;
                    }
                    let payload = ReviewPayload::Verdict {
                        round,
                        kind: VerdictKind::Blocked,
                        cumulative: zero_cumulative(),
                    };
                    self.fold_round_sent(&mut round_ledger)?;
                    round_ledger.apply(&payload)?;
                    self.append(&payload)?;
                    round_ledger.note_round_complete();
                    return Ok((round_ledger, LoopDriverStop::Blocked { role }));
                }
                Turn::Stop(stop) => return Ok((ledger, stop)),
                // Never: the reviewer's prompt is not a round boundary.
                Turn::RuleFirst(_) => unreachable!("reviewer turn is not a boundary"),
                Turn::Accepted { value, .. } => value,
            };
            if let Some(r) = &rebuttal {
                self.append(r)?;
            }
            for payload in &staged.payloads {
                self.append(payload)?;
            }
            ledger = staged.ledger;
            // Reviewer `turn_sent` only clears that role's binding notes, so
            // folding it after the verdict payloads equals replay's order.
            self.fold_round_sent(&mut ledger)?;
            // The round is fully folded: snapshot its open set (§3.3, AC9).
            // `ledger::fold_rounds` notes the same boundaries on replay, so
            // the in-memory ledger stays byte-comparable to the channel (AC11).
            ledger.note_round_complete();

            match staged.kind {
                VerdictKind::Approve => {
                    discard_held_rulings(
                        self.transport,
                        &mut self.held,
                        RULING_SESSION_END_APPROVE,
                    )?;
                    return Ok((ledger, LoopDriverStop::Approve));
                }
                VerdictKind::Blocked => {
                    discard_held_rulings(
                        self.transport,
                        &mut self.held,
                        RULING_SESSION_END_BLOCKED,
                    )?;
                    return Ok((ledger, LoopDriverStop::ReviewerBlocked));
                }
                VerdictKind::Revise => {}
            }
            // P2-§5.3: rulings held from the reviewer's prompt land after
            // the seal, before the next round's main `turn_sent`.
            let mut held = std::mem::take(&mut self.held);
            let mut ruled =
                apply_held_rulings(&self.ruling_ctx(false), &mut ledger, &mut held)? > 0;
            // §3.4 / P2-§5.1: an escalation waits for the human's ruling.
            if !ledger.pending_ruling().is_empty() {
                if let Some(stop) = self.settle(&mut ledger)? {
                    return Ok((ledger, stop));
                }
                ruled = true;
            }
            if ruled {
                self.last_activity = (self.now)();
            }
            // Round-stuck (§3.3) guards agents going in circles. A ruling is
            // the human moving the open set, so the snapshot this round
            // sealed with no longer describes it; the next seal re-judges.
            if ledger.round_stuck && !ruled {
                return Ok((ledger, LoopDriverStop::RoundStuck));
            }
            round += 1;
        }
    }

    /// Send one turn to `member` and validate the reply. §3.2 / §3.4 (AC5):
    /// a malformed reply is re-sent [`MALFORMED_RESPONSE_RETRIES`] time(s)
    /// with a validation hint naming the problem; still malformed →
    /// [`LoopDriverStop::Blocked`]. Every send goes through
    /// `run_turn_with_retry`, so the kill-switch check (A4) and the
    /// transport retry/pause (§8.1) cover the re-send too.
    fn turn<T>(
        &mut self,
        role: Role,
        round: u32,
        side: Side<'_>,
        build: impl Fn(&Ledger, &[HumanNote]) -> serde_json::Value,
        gate: TurnGate<'_>,
        validate: impl Fn(&Ledger, &str) -> std::result::Result<T, String>,
    ) -> Result<Turn<T>> {
        let member = match role {
            Role::Main => self.main,
            Role::Reviewer => self.reviewer,
        };
        let side = std::cell::RefCell::new(side);
        // Taken for the turn; put back only on `RuleFirst` (P3a-§5.2).
        let mut pending = std::mem::take(&mut self.pending);
        let mut hint: Option<String> = None;
        for attempt in 0..=MALFORMED_RESPONSE_RETRIES {
            // Only main's first send is the round boundary. A `/rule` at a
            // validation re-send prompt is held like the reviewer's: main's
            // `turn_sent` for this round is already on the channel.
            let gate = SendGate {
                boundary: role == Role::Main && attempt == 0,
                pre_confirmed: gate.pre_confirmed && attempt == 0,
                open: gate.open,
            };
            // Every prompt, resends included, is rebuilt from the ledger and
            // the pending notes (P3a-§4), plus the format hint on a resend.
            let message = |notes: &[HumanNote]| {
                let params = build(&*side.borrow().primary, notes);
                match &hint {
                    None => params,
                    Some(h) => text_message_params(&format!(
                        "{}{h}",
                        message_text(&params).unwrap_or_default()
                    )),
                }
            };
            let (svc, mur_home, channel_id) = (&self.svc, self.mur_home, self.channel_id);
            let mut flush =
                |notes: &[HumanNote]| flush_notes(svc, mur_home, channel_id, &side, notes);
            let outcome = run_turn_with_retry(
                self.transport,
                self.mur_home,
                self.fleet_name,
                member,
                &message,
                &mut pending,
                &mut flush,
                gate,
                self.channel_id,
                self.retry_delay,
            );
            let waited = self.transport.take_human_wait();
            self.human_wait += waited;
            let recorded = waited + std::mem::take(&mut self.carried_wait);
            let reply = match outcome? {
                RetryOutcome::Stopped => return Ok(Turn::Stop(LoopDriverStop::Stopped)),
                RetryOutcome::Paused { reason } => {
                    return Ok(Turn::Stop(LoopDriverStop::Paused { reason }));
                }
                RetryOutcome::TaskFailed(f) => {
                    return Ok(Turn::Stop(LoopDriverStop::TaskFailed {
                        member: f.member,
                        cause: f.cause,
                    }));
                }
                RetryOutcome::RuleFirst(r) => {
                    self.pending = pending;
                    self.carried_wait += recorded;
                    return Ok(Turn::RuleFirst(r));
                }
                RetryOutcome::Sent { reply, held } => {
                    self.held.extend(held);
                    reply
                }
            };
            let sent = append_turn_sent(
                &self.svc,
                self.mur_home,
                self.channel_id,
                round,
                role,
                recorded,
            )?;
            self.round_sent.push(sent);
            if let Some(stop) = check_guards(
                round,
                self.elapsed(),
                self.deadline,
                Duration::ZERO,
                Stuck::Off,
            ) {
                return Ok(Turn::Stop(LoopDriverStop::Guard(stop)));
            }
            self.last_activity = (self.now)();
            let checked = validate(&*side.borrow().primary, &reply);
            match checked {
                Ok(value) => return Ok(Turn::Accepted { reply, value }),
                Err(problem) if attempt < MALFORMED_RESPONSE_RETRIES => {
                    hint = Some(REVIEW_VALIDATION_HINT.replace("{problem}", &problem));
                }
                Err(_) => {}
            }
        }
        Ok(Turn::Stop(LoopDriverStop::Blocked { role }))
    }

    /// Fold this round's buffered `turn_sent` events into `ledger`.
    fn fold_round_sent(&mut self, ledger: &mut Ledger) -> Result<()> {
        for sent in self.round_sent.drain(..) {
            ledger.apply(&sent)?;
        }
        Ok(())
    }

    fn append(&self, payload: &ReviewPayload) -> Result<()> {
        append(&self.svc, self.mur_home, self.channel_id, payload)
    }

    fn ruling_ctx(&self, already_paused: bool) -> RulingCtx<'_> {
        RulingCtx {
            transport: self.transport,
            svc: &self.svc,
            mur_home: self.mur_home,
            fleet_name: self.fleet_name,
            channel_id: self.channel_id,
            already_paused,
        }
    }

    /// Ask for every owed ruling (P2-§5.1). `None` → settled, continue.
    /// Prompt time is human wait: out of execution time, carried onto the
    /// next `turn_sent` (AC-P2-7).
    fn settle(&mut self, ledger: &mut Ledger) -> Result<Option<LoopDriverStop>> {
        let cumulative = Cumulative {
            exec_time_ms: u64::try_from(self.elapsed().as_millis()).unwrap_or(u64::MAX),
            cost_usd_micros: ledger.cost_usd_micros,
        };
        let outcome = settle_rulings(
            &self.ruling_ctx(false),
            ledger,
            cumulative,
            self.carried_wait,
        )?;
        let waited = self.transport.take_human_wait();
        self.human_wait += waited;
        self.carried_wait += waited;
        Ok(match outcome {
            RulingOutcome::Settled => None,
            RulingOutcome::LeftPaused => Some(LoopDriverStop::Paused {
                reason: REVIEW_PAUSE_REASON_ESCALATION.to_string(),
            }),
            RulingOutcome::Abandoned => Some(LoopDriverStop::Escalation),
            RulingOutcome::KillSwitch => Some(LoopDriverStop::Stopped),
        })
    }
}

/// P3a-§5.3: append each consented note in queue order, then fold it per
/// the two-ledger rule. An append failure stops the flush there and
/// reports how many notes made it ([`FlushFailed`]); the driver pauses.
fn flush_notes(
    svc: &ChannelService,
    mur_home: &Path,
    channel_id: &str,
    side: &std::cell::RefCell<Side<'_>>,
    notes: &[HumanNote],
) -> Result<()> {
    for (recorded, note) in notes.iter().enumerate() {
        let payload = ReviewPayload::from(note.clone());
        if let Err(e) = append(svc, mur_home, channel_id, &payload) {
            return Err(FlushFailed {
                recorded,
                total: notes.len(),
                cause: e.to_string(),
            }
            .into());
        }
        let mut side = side.borrow_mut();
        side.primary.apply(&payload)?;
        if let Some(also) = side.also.as_deref_mut() {
            also.apply(&payload)?;
        }
    }
    Ok(())
}

/// The open set's IDs: what a `/rule` line is validated against (P2-§5.4).
fn open_ids(ledger: &Ledger) -> std::collections::BTreeSet<String> {
    ledger.open_set().into_keys().collect()
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
    human_wait: Duration,
) -> Result<ReviewPayload> {
    let payload = ReviewPayload::TurnSent {
        round,
        to,
        restart_note: None,
        human_wait_ms: u64::try_from(human_wait.as_millis()).unwrap_or(u64::MAX),
    };
    append(svc, mur_home, channel_id, &payload)?;
    Ok(payload)
}
