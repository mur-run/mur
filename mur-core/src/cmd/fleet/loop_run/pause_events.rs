//! Channel events that record a fleet run's pause and resume (pause/resume
//! design §5.6).
//!
//! Encoding follows the review driver's precedent (`review/schema.rs`): no new
//! `EventKind` variant exists, the events ride inside `EventKind::Note` as
//! `Note.payload == {"fleet_run": {"v": 1, "type": "<kind>", ...}}`. They are
//! deliberately NOT `StateChange`s, so `channel_terminal_status` (`jobs.rs`)
//! ignores them and a paused job is never read as canceled.
//! `run_status::rebuild` does read them, but only as the non-terminal
//! `Paused` / `Running`. A `Note` with no `"fleet_run"` key is an
//! ordinary note; one that HAS the key but does not deserialize is damage, not
//! an ignorable note.

use serde::{Deserialize, Serialize};

/// The top-level key inside `Note.payload` that marks a fleet-run event.
pub const FLEET_RUN_PAYLOAD_KEY: &str = "fleet_run";
/// Schema version stamped on every payload, so a future shape change is not
/// silently misread as today's.
pub const FLEET_RUN_SCHEMA_VERSION: u32 = 1;

/// Why a run paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// `mur fleet pause`.
    Operator,
}

/// The bounds a run resolved at its original start (rev 3, §5.3). Carried on
/// the signed `run_paused` so resume has an authority for them that the run
/// itself cannot edit — the checkpoint copies are convenience only.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RunBounds {
    pub deadline_ms: u64,
    /// `None` = stuck detection off.
    pub stuck_ms: Option<u64>,
    /// `None` = no cost cap.
    pub cost_usd: Option<f64>,
}

/// One pause/resume event. `spent_usd` and `active_elapsed_ms` are cumulative
/// and are adopted with `max()` on resume (§5.3), so nothing moves down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FleetRunPayload {
    RunPaused {
        run_id: String,
        iteration_run_id: String,
        iteration: u32,
        spent_usd: f64,
        active_elapsed_ms: u64,
        reason: PauseReason,
        /// Absent on events written before rev 3; resume then falls back to
        /// the checkpoint and warns (§5.3).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bounds: Option<RunBounds>,
    },
    RunResumed {
        run_id: String,
        iteration_run_id: String,
        iteration: u32,
        spent_usd: f64,
        active_elapsed_ms: u64,
    },
}

/// The versioned wrapper stored under [`FLEET_RUN_PAYLOAD_KEY`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetRunEnvelope {
    pub v: u32,
    #[serde(flatten)]
    pub payload: FleetRunPayload,
}

impl FleetRunPayload {
    /// The full `Note.payload` for this event.
    pub fn to_note_payload(&self) -> serde_json::Value {
        let env = FleetRunEnvelope {
            v: FLEET_RUN_SCHEMA_VERSION,
            payload: self.clone(),
        };
        serde_json::json!({ FLEET_RUN_PAYLOAD_KEY: env })
    }
}

/// What a `Note`'s payload is, as far as fleet-run events go.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteClassification {
    /// No `"fleet_run"` key: an ordinary note, ignored by the fold.
    NotFleetRun,
    FleetRun(FleetRunEnvelope),
    /// Has the key but is unreadable (damage, or a newer schema).
    Malformed,
}

pub fn classify_note_payload(payload: &serde_json::Value) -> NoteClassification {
    let Some(inner) = payload.get(FLEET_RUN_PAYLOAD_KEY) else {
        return NoteClassification::NotFleetRun;
    };
    match serde_json::from_value::<FleetRunEnvelope>(inner.clone()) {
        Ok(env) if env.v == FLEET_RUN_SCHEMA_VERSION => NoteClassification::FleetRun(env),
        _ => NoteClassification::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paused() -> FleetRunPayload {
        FleetRunPayload::RunPaused {
            run_id: "r".into(),
            iteration_run_id: "r-3".into(),
            iteration: 3,
            spent_usd: 1.84,
            active_elapsed_ms: 412_000,
            reason: PauseReason::Operator,
            bounds: Some(RunBounds {
                deadline_ms: 1_800_000,
                stuck_ms: None,
                cost_usd: Some(5.0),
            }),
        }
    }

    #[test]
    fn run_paused_round_trips_through_a_note_payload() {
        let note = paused().to_note_payload();
        assert_eq!(note["fleet_run"]["v"], 1);
        assert_eq!(note["fleet_run"]["type"], "run_paused");
        assert_eq!(note["fleet_run"]["reason"], "operator");
        match classify_note_payload(&note) {
            NoteClassification::FleetRun(env) => assert_eq!(env.payload, paused()),
            other => panic!("expected FleetRun, got {other:?}"),
        }
    }

    #[test]
    fn a_run_paused_without_bounds_still_reads_as_a_fleet_run_event() {
        let mut note = paused().to_note_payload();
        note["fleet_run"].as_object_mut().unwrap().remove("bounds");
        match classify_note_payload(&note) {
            NoteClassification::FleetRun(FleetRunEnvelope {
                payload: FleetRunPayload::RunPaused { bounds, .. },
                ..
            }) => assert_eq!(bounds, None),
            other => panic!("expected RunPaused, got {other:?}"),
        }
    }

    #[test]
    fn run_resumed_round_trips_and_has_no_reason() {
        let resumed = FleetRunPayload::RunResumed {
            run_id: "r".into(),
            iteration_run_id: "r-3".into(),
            iteration: 3,
            spent_usd: 1.84,
            active_elapsed_ms: 412_000,
        };
        let note = resumed.to_note_payload();
        assert_eq!(note["fleet_run"]["type"], "run_resumed");
        assert!(note["fleet_run"].get("reason").is_none());
        assert_eq!(
            classify_note_payload(&note),
            NoteClassification::FleetRun(FleetRunEnvelope {
                v: 1,
                payload: resumed
            })
        );
    }

    #[test]
    fn an_ordinary_note_is_not_a_fleet_run_event() {
        let ordinary = serde_json::json!({ "text": "budget exhausted" });
        assert_eq!(
            classify_note_payload(&ordinary),
            NoteClassification::NotFleetRun
        );
        // A review event shares `Note` but must not be mistaken for ours.
        let review = serde_json::json!({ "review": { "v": 1, "type": "paused" } });
        assert_eq!(
            classify_note_payload(&review),
            NoteClassification::NotFleetRun
        );
    }

    #[test]
    fn a_damaged_or_future_schema_payload_is_malformed_not_ignored() {
        let damaged = serde_json::json!({ "fleet_run": { "v": 1, "type": "run_paused" } });
        assert_eq!(
            classify_note_payload(&damaged),
            NoteClassification::Malformed
        );
        let mut future = paused().to_note_payload();
        future["fleet_run"]["v"] = serde_json::json!(2);
        assert_eq!(
            classify_note_payload(&future),
            NoteClassification::Malformed
        );
    }
}
