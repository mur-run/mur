//! §2.3 A2 / §3.1: the transport seam between the review driver and A2A, and
//! one turn of the two-party protocol built on it.
//!
//! The driver reuses `crate::a2a_dial::dial_message_streaming` unchanged
//! (A2: "It reuses the existing pieces unchanged ... and the channel
//! service") and checks `control::is_stopped` BEFORE every send (A4: "the
//! kill-switch is checked per turn, not per iteration — for the review
//! driver only"). A stop observed before a send means the send never
//! happens. A stop arriving mid-send is a driver-level concern beyond one
//! `run_turn` call (A4: "the in-flight A2A call is not cancelled ... after
//! it returns, no further send happens") — this module's contract is only
//! the pre-send check; the caller is responsible for not calling `run_turn`
//! again once a turn reports `Stopped`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};

use super::constants::NOTE_FLUSH_FAILED_REASON;
use super::ledger::{EscalationRecord, Ledger};
use super::ruling::RulingInput;
use super::schema::{Cumulative, HumanNote, Mode, PauseKind, ReviewPayload, to_note_payload};
use crate::cmd::fleet::control;

/// Send one A2A message to a named fleet member and return its reply text,
/// or an error on transport failure (§8.1). Implemented once for real A2A
/// ([`A2aTransport`]) and once as a stub for tests (`driver_tests.rs`).
pub trait ReviewTransport {
    fn send(&self, member: &str, params: &serde_json::Value) -> Result<String>;

    /// §5 semi-auto / P2-§5.3: the human gate before a send. Called after
    /// the `.stopped` check and before [`ReviewTransport::send`]. `Stop`
    /// ends the turn as `Stopped` with nothing sent; `SendWithRuling`
    /// carries a `/rule` line validated against `open`. The default lets
    /// every send through (tests, and any caller that has already gated
    /// elsewhere).
    fn confirm_send(
        &self,
        _member: &str,
        _params: &serde_json::Value,
        _open: &BTreeSet<String>,
    ) -> Result<SendAnswer> {
        Ok(SendAnswer::Send)
    }

    /// P2-§5.1 step 3: show `pending` with both sides' last positions and
    /// read one raw line (unparsed, so the kill-switch check runs first).
    /// Default (tests, non-terminal): EOF, which leaves the session paused.
    fn ask_ruling(&self, _pending: &EscalationRecord, _ledger: &Ledger) -> Result<String> {
        Ok(String::new())
    }

    /// P2-§5: print one line or block to the human (ruling notices, the
    /// rebuilt message, an inline hint). Default: nowhere.
    fn show(&self, _text: &str) -> Result<()> {
        Ok(())
    }

    /// §3.5 human-input wait: time spent waiting on the human since the
    /// last call (send prompts, tool-approval prompts), then reset. The
    /// loop takes it out of execution time. Default: no human, no wait.
    fn take_human_wait(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    /// P3b-§4.2 / D7: whether the turn that just returned from
    /// [`ReviewTransport::send`] was committed. `false` means the human's
    /// Esc×2 won the race: the reply is dropped and the turn ends
    /// [`RetryOutcome::Aborted`]. Default `true`; only the MURMUR transport
    /// overrides it.
    fn turn_committed(&self) -> bool {
        true
    }

    /// P3b-§4.4: when the last `Stop` was a pause this transport asked for
    /// (Esc×1, or the UI side gone) rather than the kill-switch, say so:
    /// the session then records `paused` instead of `session_stopped`.
    /// Default `None`; only the MURMUR transport overrides it.
    fn take_requested_pause(&self) -> Option<RequestedPause> {
        None
    }
}

/// A pause the transport asked for (see [`ReviewTransport::take_requested_pause`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedPause {
    pub kind: PauseKind,
    pub reason: &'static str,
    /// Human wait not yet recorded on any event (P3b-D8).
    pub human_wait: Duration,
}

/// The human's answer at the send prompt (P2-§5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendAnswer {
    Send,
    Stop,
    /// A `/rule` line typed instead of Enter: it is the send consent too.
    SendWithRuling(RulingInput),
    /// P3a-§5.1: `/note` or `@<agent>` — queued, never send consent (N2).
    /// The driver rebuilds the message with it and asks again.
    Note(HumanNote),
}

/// P3a-§5.3: the flush callback could only append `recorded` of `total`
/// pending notes. The driver turns this into a `paused { kind: other }`
/// with [`NOTE_FLUSH_FAILED_REASON`]; any other callback error propagates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushFailed {
    pub recorded: usize,
    pub total: usize,
    pub cause: String,
}

impl std::fmt::Display for FlushFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            &NOTE_FLUSH_FAILED_REASON
                .replace("{recorded}", &self.recorded.to_string())
                .replace("{total}", &self.total.to_string())
                .replace("{cause}", &self.cause),
        )
    }
}

impl std::error::Error for FlushFailed {}

/// How one turn is gated. `open` is the open set a `/rule` line is
/// validated against.
#[derive(Debug, Clone, Copy)]
pub struct SendGate<'a> {
    /// True only for main's turn: a ruling typed there is applied before
    /// the send (P2-§5.3), so nothing is sent this call.
    pub boundary: bool,
    /// Skip `confirm_send`: the human already consented (the rebuilt main
    /// send after a boundary ruling).
    pub pre_confirmed: bool,
    pub open: &'a BTreeSet<String>,
}

/// A member's turn ended with its task `failed` or `cancelled` — a real
/// answer from a live agent, not a transport fault. Distinct from a send
/// error so [`run_turn_with_retry`] does not re-run the whole turn (and so
/// the stop screen does not report it as a malformed verdict). The runtime
/// returns such a task as a *successful* JSON-RPC result; this is where it
/// becomes an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFailed {
    pub member: String,
    pub cause: String,
}

impl std::fmt::Display for TaskFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} task failed: {}", self.member, self.cause)
    }
}

impl std::error::Error for TaskFailed {}

/// Answers one gated tool call raised inside a member's turn:
/// `(member, call) -> allow`. The call is one entry of the runtime's
/// approval notification (`hitl_id`, `tool_name`, `tool_input`, `risk`, …);
/// a batched notification is split by [`answer_hitl`] first.
pub type HitlDecider<'a> = &'a dyn Fn(&str, &serde_json::Value) -> bool;

/// Real transport: wraps [`crate::a2a_dial::dial_message_streaming`]
/// unchanged (A2). Accumulates only non-thinking deltas as the reply text,
/// the same pattern `loop_run::synth::ask_router_done` already uses for a
/// one-shot streamed reply.
///
/// Every HITL request is answered through `decide` (§9: "the HITL gate
/// inside each member's turn is unchanged" — it still gates; this only
/// makes sure the question reaches someone). Dropping the request, as this
/// transport once did, left the runtime waiting until its own timeout and
/// denied every gated tool call.
pub struct A2aTransport<'a> {
    pub mur_home: &'a Path,
    pub decide: HitlDecider<'a>,
}

/// Answer one `tool/approval_needed` notification: ask `decide` once per
/// gated call and hand each `(hitl_id, allow)` to `respond`.
///
/// A runtime that gates several calls in one step sends them as `calls`,
/// each with its own `hitl_id`; the top-level single-call fields mirror only
/// `calls[0]` for older clients. Answering just the top-level id leaves the
/// rest to the runtime's approval timeout, which denies them and fails the
/// member's task. Same split as murmur's `HitlRequest::from_params`.
pub(super) fn answer_hitl(
    member: &str,
    hitl: &serde_json::Value,
    decide: HitlDecider<'_>,
    mut respond: impl FnMut(&str, bool),
) {
    let calls: Vec<&serde_json::Value> =
        match hitl.get("calls").and_then(serde_json::Value::as_array) {
            Some(calls) if !calls.is_empty() => calls.iter().collect(),
            _ => vec![hitl],
        };
    for call in calls {
        let id = call
            .get("hitl_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        respond(id, decide(member, call));
    }
}

/// Audit attribution for an answer given at the review session's terminal.
const HITL_SURFACE: &str = "cli";

impl ReviewTransport for A2aTransport<'_> {
    fn send(&self, member: &str, params: &serde_json::Value) -> Result<String> {
        let mut streamed = String::new();
        let task = crate::a2a_dial::dial_message_streaming(
            self.mur_home,
            member,
            params.clone(),
            |delta, thinking, _id| {
                if !thinking {
                    streamed.push_str(delta);
                }
            },
            |hitl| {
                answer_hitl(member, &hitl, self.decide, |id, allow| {
                    if let Err(e) = crate::a2a_dial::dial_method(
                        self.mur_home,
                        member,
                        "tool/hitl_respond",
                        crate::cmd::agent::cli::stream::hitl_respond_params(
                            self.mur_home,
                            id,
                            allow,
                            HITL_SURFACE,
                        ),
                        crate::a2a_dial::DialMode::RequireRunning,
                    ) {
                        tracing::warn!(member, error = %e, "could not deliver the HITL answer");
                    }
                });
            },
            |_step| {},
        )?;
        task_reply(member, &task, streamed)
    }
}

/// A failed/cancelled task becomes [`TaskFailed`]; otherwise the task's
/// final reply, falling back to the streamed deltas when it is empty (the
/// same fallback `loop_run::synth` uses).
pub(super) fn task_reply(
    member: &str,
    task: &serde_json::Value,
    streamed: String,
) -> Result<String> {
    if let Err(cause) = crate::cmd::agent::cli::stream::task_outcome(task) {
        return Err(TaskFailed {
            member: member.to_string(),
            cause,
        }
        .into());
    }
    let final_reply = crate::cmd::fleet::loop_run::synth::extract_task_reply(task);
    Ok(if final_reply.trim().is_empty() {
        streamed
    } else {
        final_reply
    })
}

/// Outcome of one [`run_turn`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The kill-switch was engaged before the send, or the human declined;
    /// nothing was sent.
    Stopped,
    /// Exactly one send happened; `held` is a ruling typed at a
    /// non-boundary (reviewer) prompt, applied after the round seals.
    Sent {
        reply: String,
        held: Option<RulingInput>,
    },
    /// `/rule` at a boundary (main) prompt: nothing sent; the loop applies
    /// the ruling and re-sends the rebuilt message.
    RuleFirst(RulingInput),
}

/// What the send gate decided, before any send.
enum Gated {
    Stop,
    RuleFirst(RulingInput),
    Go(Option<RulingInput>),
    Note(HumanNote),
}

fn gate(
    transport: &dyn ReviewTransport,
    member: &str,
    params: &serde_json::Value,
    g: SendGate,
) -> Result<Gated> {
    if g.pre_confirmed {
        return Ok(Gated::Go(None));
    }
    Ok(match transport.confirm_send(member, params, g.open)? {
        SendAnswer::Stop => Gated::Stop,
        SendAnswer::Send => Gated::Go(None),
        SendAnswer::SendWithRuling(r) if g.boundary => Gated::RuleFirst(r),
        SendAnswer::SendWithRuling(r) => Gated::Go(Some(r)),
        SendAnswer::Note(n) => Gated::Note(n),
    })
}

/// One turn of the two-party protocol (§3.1): checks `.stopped` for
/// `fleet_name` BEFORE sending (A4) and, only if clear and the gate lets
/// it through, sends exactly one A2A message to `member` via `transport`.
/// Never sends after observing a stop. The inner step of
/// [`run_turn_with_retry`]: `params` are fixed, so it has no note queue —
/// a `Note` answer here is an error (callers that prompt use the retry
/// form, which rebuilds the message).
pub fn run_turn(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    fleet_name: &str,
    member: &str,
    params: &serde_json::Value,
    g: SendGate,
) -> Result<TurnOutcome> {
    if control::is_stopped(mur_home, fleet_name) {
        return Ok(TurnOutcome::Stopped);
    }
    let held = match gate(transport, member, params, g)? {
        Gated::Stop => return Ok(TurnOutcome::Stopped),
        Gated::RuleFirst(r) => return Ok(TurnOutcome::RuleFirst(r)),
        Gated::Go(held) => held,
        Gated::Note(_) => anyhow::bail!("run_turn has no note queue; use run_turn_with_retry"),
    };
    let reply = transport.send(member, params)?;
    Ok(TurnOutcome::Sent { reply, held })
}

/// Outcome of [`run_turn_with_retry`] (§8.1, AC14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryOutcome {
    /// `.stopped` was observed before a send (first attempt or the retry),
    /// or the human declined; no `paused` event is written — a stop is its
    /// own, separate stop path.
    Stopped,
    /// A send succeeded (first attempt or the retry). See
    /// [`TurnOutcome::Sent`] for `held`.
    Sent {
        reply: String,
        held: Option<RulingInput>,
    },
    /// See [`TurnOutcome::RuleFirst`].
    RuleFirst(RulingInput),
    /// The first send failed, the retry (after `retry_delay`) also failed:
    /// a signed `paused` event (with `reason`) and a `mode_changed` event
    /// reverting to semi-auto were written to `channel_id`.
    Paused { reason: String },
    /// The member answered, but its task ended `failed`/`cancelled`. Not
    /// retried: the send worked, and re-running a turn is not free.
    TaskFailed(TaskFailed),
    /// P3b-§6.3: the human's abort won before `send` returned. The reply is
    /// dropped, nothing is ledgered, no retry. Never `TaskFailed`, never a
    /// transport failure; the caller writes the `paused` event.
    Aborted,
}

/// §8.1 / AC14: "A2A send failure or peer offline → one retry after a
/// configured delay → still failing → pause, revert to semi-auto, and show
/// the reason."
///
/// P3a-§4/§5: the message is `build(pending)`, rebuilt on every prompt so a
/// `Note` answer is queued and shown before the human is asked again (N5:
/// the reprinted and the sent message are one generator call). On consent
/// (`Go`, or `pre_confirmed` — the send after a boundary `/rule`, D1)
/// `on_consented(pending)` flushes the queue, which is then cleared (N9),
/// and the same `params` are sent. `Stop` leaves `pending` for the caller
/// to discard; `RuleFirst` keeps it. A [`FlushFailed`] from the callback
/// pauses with `kind: other` and sends nothing (§5.3).
///
/// `.stopped` is checked before every send, the retry included (A4) — a
/// stop observed on either attempt returns `Stopped` immediately and writes
/// no `paused` event, since a kill-switch stop is a distinct stop path, not
/// a transport failure. The gate is not asked again, and `on_consented` is
/// not called again, on the transport retry: it re-sends the same `params`
/// under the first answer, including any `held` ruling (P2 Task 6).
/// `retry_delay` is an explicit parameter (not read from
/// `constants::TRANSPORT_RETRY_DELAY` directly) so tests can pass
/// `Duration::ZERO` and never sleep for real; production callers pass the
/// named constant.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_with_retry(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    fleet_name: &str,
    member: &str,
    build: &dyn Fn(&[HumanNote]) -> serde_json::Value,
    pending: &mut Vec<HumanNote>,
    on_consented: &mut dyn FnMut(&[HumanNote]) -> Result<()>,
    g: SendGate,
    channel_id: &str,
    retry_delay: Duration,
) -> Result<RetryOutcome> {
    let (params, held) = loop {
        if control::is_stopped(mur_home, fleet_name) {
            return Ok(RetryOutcome::Stopped);
        }
        let params = build(pending);
        match gate(transport, member, &params, g)? {
            Gated::Stop => return Ok(RetryOutcome::Stopped),
            Gated::RuleFirst(r) => return Ok(RetryOutcome::RuleFirst(r)),
            Gated::Note(n) => pending.push(n),
            Gated::Go(held) => break (params, held),
        }
    };
    if let Err(e) = on_consented(pending) {
        let Some(failed) = e.downcast_ref::<FlushFailed>() else {
            return Err(e);
        };
        let reason = failed.to_string();
        let wait = transport.take_human_wait();
        write_paused_and_revert(mur_home, channel_id, PauseKind::Other, &reason, wait)?;
        transport.show(&reason)?;
        return Ok(RetryOutcome::Paused { reason });
    }
    pending.clear();
    // Consent is given; each attempt still re-checks `.stopped` (A4).
    let confirmed = SendGate {
        pre_confirmed: true,
        ..g
    };
    // P3b-§4.2: `turn_committed()` is read right after `send` returns,
    // before the `Result` is looked at — an abort that won the race turns
    // any reply or error (a cancelled task included) into `Aborted`.
    let attempt = || {
        let out = run_turn(transport, mur_home, fleet_name, member, &params, confirmed);
        if transport.turn_committed() {
            Some(out)
        } else {
            None
        }
    };
    let sent = |outcome| match outcome {
        TurnOutcome::Sent { reply, .. } => RetryOutcome::Sent {
            reply,
            held: held.clone(),
        },
        _ => RetryOutcome::Stopped,
    };
    match attempt() {
        None => return Ok(RetryOutcome::Aborted),
        Some(Ok(outcome)) => return Ok(sent(outcome)),
        Some(Err(first_err)) => {
            if let Some(failed) = first_err.downcast_ref::<TaskFailed>() {
                return Ok(RetryOutcome::TaskFailed(failed.clone()));
            }
        }
    }

    std::thread::sleep(retry_delay);

    match attempt() {
        None => Ok(RetryOutcome::Aborted),
        Some(Ok(outcome)) => Ok(sent(outcome)),
        Some(Err(second_err)) => {
            if let Some(failed) = second_err.downcast_ref::<TaskFailed>() {
                return Ok(RetryOutcome::TaskFailed(failed.clone()));
            }
            let reason = format!("transport failure after one retry: {second_err}");
            let wait = transport.take_human_wait();
            write_paused_and_revert(mur_home, channel_id, PauseKind::Transport, &reason, wait)?;
            Ok(RetryOutcome::Paused { reason })
        }
    }
}

/// Write the `paused` event (with `reason` and zeroed cumulative — the
/// driver's caller owns the real running totals and is expected to fold
/// them in before this point in the full loop; D2 lands the retry/pause
/// mechanism in isolation, D3 wires it into a running session) and the
/// `mode_changed` event reverting to semi-auto (§5, §8.1), both signed when
/// the fleet's writer identity is available (migration-safe fallback to
/// unsigned otherwise, same as every other channel writer in this crate).
///
/// `human_wait` is the gate / HITL wait not yet recorded on a `turn_sent`
/// (P3b-D8); it lands in `paused.human_wait_ms` so a resume does not count
/// it as execution time against the deadline.
pub(super) fn write_paused_and_revert(
    mur_home: &Path,
    channel_id: &str,
    kind: PauseKind,
    reason: &str,
    human_wait: Duration,
) -> Result<()> {
    let svc = ChannelService::open(mur_home)?;
    let zero = Cumulative {
        exec_time_ms: 0,
        cost_usd_micros: 0,
    };
    crate::channel_writer::append_as_writer(
        &svc,
        mur_home,
        channel_id,
        crate::channel_writer::ROUTER_AGENT,
        ChannelActor::System,
        EventKind::Note,
        to_note_payload(&ReviewPayload::Paused {
            kind,
            reason: reason.to_string(),
            cumulative: zero,
            human_wait_ms: u64::try_from(human_wait.as_millis()).unwrap_or(u64::MAX),
        }),
        None,
    )?;
    crate::channel_writer::append_as_writer(
        &svc,
        mur_home,
        channel_id,
        crate::channel_writer::ROUTER_AGENT,
        ChannelActor::System,
        EventKind::Note,
        to_note_payload(&ReviewPayload::ModeChanged {
            mode: Mode::SemiAuto,
        }),
        None,
    )?;
    Ok(())
}
