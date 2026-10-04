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

use anyhow::Result;

use crate::cmd::fleet::control;

/// Send one A2A message to a named fleet member and return its reply text,
/// or an error on transport failure (§8.1). Implemented once for real A2A
/// ([`A2aTransport`]) and once as a stub for tests (`driver_tests.rs`).
pub trait ReviewTransport {
    fn send(&self, member: &str, params: &serde_json::Value) -> Result<String>;
}

/// Real transport: wraps [`crate::a2a_dial::dial_message_streaming`]
/// unchanged (A2). Accumulates only non-thinking deltas as the reply text,
/// the same pattern `loop_run::synth::ask_router_done` already uses for a
/// one-shot streamed reply.
pub struct A2aTransport<'a> {
    pub mur_home: &'a Path,
}

impl ReviewTransport for A2aTransport<'_> {
    fn send(&self, member: &str, params: &serde_json::Value) -> Result<String> {
        let mut streamed = String::new();
        crate::a2a_dial::dial_message_streaming(
            self.mur_home,
            member,
            params.clone(),
            |delta, thinking, _id| {
                if !thinking {
                    streamed.push_str(delta);
                }
            },
            |_hitl| {},
            |_step| {},
        )?;
        Ok(streamed)
    }
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
    let reply = transport.send(member, params)?;
    Ok(TurnOutcome::Sent(reply))
}
