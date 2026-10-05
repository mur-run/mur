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

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};

use super::schema::{Cumulative, Mode, PauseKind, ReviewPayload, to_note_payload};
use crate::cmd::fleet::control;

/// Send one A2A message to a named fleet member and return its reply text,
/// or an error on transport failure (§8.1). Implemented once for real A2A
/// ([`A2aTransport`]) and once as a stub for tests (`driver_tests.rs`).
pub trait ReviewTransport {
    fn send(&self, member: &str, params: &serde_json::Value) -> Result<String>;

    /// §5 semi-auto: the human gate before a send. Called after the
    /// `.stopped` check and before [`ReviewTransport::send`]; `false` means
    /// the human declined, and the turn ends as `Stopped` with nothing sent.
    /// The default lets every send through (tests, and any caller that has
    /// already gated elsewhere).
    fn confirm_send(&self, _member: &str, _params: &serde_json::Value) -> Result<bool> {
        Ok(true)
    }

    /// §3.5 human-input wait: time spent waiting on the human since the
    /// last call (send prompts, tool-approval prompts), then reset. The
    /// loop takes it out of execution time. Default: no human, no wait.
    fn take_human_wait(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
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

/// Answers a tool-approval (HITL) request raised inside a member's turn:
/// `(member, request) -> allow`. The request is the runtime's raw
/// `tool/hitl_request` params (`hitl_id`, `tool_name`, `tool_input`, …).
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
                let id = hitl
                    .get("hitl_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let allow = (self.decide)(member, &hitl);
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
    /// The kill-switch was engaged before the send; nothing was sent.
    Stopped,
    /// Exactly one send happened; this is its reply text.
    Sent(String),
}

/// One turn of the two-party protocol (§3.1): checks `.stopped` for
/// `fleet_name` BEFORE sending (A4) and, only if clear, sends exactly one
/// A2A message to `member` via `transport`. Never sends after observing a
/// stop.
pub fn run_turn(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    fleet_name: &str,
    member: &str,
    params: &serde_json::Value,
) -> Result<TurnOutcome> {
    if control::is_stopped(mur_home, fleet_name) {
        return Ok(TurnOutcome::Stopped);
    }
    if !transport.confirm_send(member, params)? {
        return Ok(TurnOutcome::Stopped);
    }
    let reply = transport.send(member, params)?;
    Ok(TurnOutcome::Sent(reply))
}

/// Outcome of [`run_turn_with_retry`] (§8.1, AC14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryOutcome {
    /// `.stopped` was observed before a send (first attempt or the retry);
    /// no `paused` event is written — a stop is its own, separate stop path.
    Stopped,
    /// The first send succeeded; no retry happened.
    Sent(String),
    /// The first send failed, the retry (after `retry_delay`) also failed:
    /// a signed `paused` event (with `reason`) and a `mode_changed` event
    /// reverting to semi-auto were written to `channel_id`.
    Paused { reason: String },
    /// The member answered, but its task ended `failed`/`cancelled`. Not
    /// retried: the send worked, and re-running a turn is not free.
    TaskFailed(TaskFailed),
}

/// §8.1 / AC14: "A2A send failure or peer offline → one retry after a
/// configured delay → still failing → pause, revert to semi-auto, and show
/// the reason."
///
/// Each attempt goes through [`run_turn`], so `.stopped` is still checked
/// before every send (A4) — a stop observed on either attempt returns
/// `Stopped` immediately and writes no `paused` event, since a kill-switch
/// stop is a distinct stop path, not a transport failure. `retry_delay` is
/// an explicit parameter (not read from `constants::TRANSPORT_RETRY_DELAY`
/// directly) so tests can pass `Duration::ZERO` and never sleep for real;
/// production callers pass the named constant.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_with_retry(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    fleet_name: &str,
    member: &str,
    params: &serde_json::Value,
    channel_id: &str,
    retry_delay: Duration,
) -> Result<RetryOutcome> {
    match run_turn(transport, mur_home, fleet_name, member, params) {
        Ok(TurnOutcome::Stopped) => return Ok(RetryOutcome::Stopped),
        Ok(TurnOutcome::Sent(reply)) => return Ok(RetryOutcome::Sent(reply)),
        Err(first_err) => {
            if let Some(failed) = first_err.downcast_ref::<TaskFailed>() {
                return Ok(RetryOutcome::TaskFailed(failed.clone()));
            }
        }
    }

    std::thread::sleep(retry_delay);

    match run_turn(transport, mur_home, fleet_name, member, params) {
        Ok(TurnOutcome::Stopped) => Ok(RetryOutcome::Stopped),
        Ok(TurnOutcome::Sent(reply)) => Ok(RetryOutcome::Sent(reply)),
        Err(second_err) => {
            if let Some(failed) = second_err.downcast_ref::<TaskFailed>() {
                return Ok(RetryOutcome::TaskFailed(failed.clone()));
            }
            let reason = format!("transport failure after one retry: {second_err}");
            write_paused_and_revert(mur_home, channel_id, &reason)?;
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
fn write_paused_and_revert(mur_home: &Path, channel_id: &str, reason: &str) -> Result<()> {
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
            kind: PauseKind::Transport,
            reason: reason.to_string(),
            cumulative: zero,
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
