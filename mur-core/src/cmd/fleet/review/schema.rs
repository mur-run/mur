//! §4: the review event payloads stored inside `EventKind::Note` (Q4
//! decided — no new `EventKind` variant exists). Every logical event type
//! listed in §4 is a variant of [`ReviewPayload`], wrapped with a schema
//! version so the fold can tell a review event from an ordinary `Note` and
//! from a payload shape it no longer understands.
//!
//! Encoding: `Note.payload == {"review": {"v": 1, "type": "<kind>", ...}}`.
//! A `Note` with no `"review"` key is an ordinary note and is ignored by the
//! fold (§4). A `Note` that HAS the key but fails to deserialize as
//! [`ReviewEnvelope`] is damage (§8.2), not an ignorable note.

use serde::{Deserialize, Serialize};

/// The top-level key inside `Note.payload` that marks a review event.
pub const REVIEW_PAYLOAD_KEY: &str = "review";
/// Schema version stamped on every review payload (§4). Bumping this is how
/// a future shape change avoids being silently misread as today's shape.
pub const REVIEW_SCHEMA_VERSION: u32 = 1;

/// Which of the two session members a value refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Main,
    Reviewer,
}

/// §5 auto vs semi-auto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    SemiAuto,
    Auto,
}

/// §3.2 verdict severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
}

/// §3.2 verdict kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    Approve,
    Revise,
    Blocked,
}

/// §3.3 finding status, as reported by the reviewer for a prior finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Open,
    Withdrawn,
    Resolved,
    Disputed,
}

impl FindingStatus {
    /// §3.3: "Open set = `open` ∪ `disputed`."
    pub fn is_open_set(self) -> bool {
        matches!(self, FindingStatus::Open | FindingStatus::Disputed)
    }

    /// §3.3: "Closed states: `withdrawn`, `resolved`, closed by `/rule`."
    pub fn is_closed(self) -> bool {
        !self.is_open_set()
    }
}

/// §3.4 main-agent rebuttal answer for one open finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RebuttalAnswer {
    Accept,
    Reject,
    Partial,
}

/// §3.2: one new finding raised in a verdict. IDs are NEVER taken from the
/// model — whatever the model put in its own field (if anything) is parsed
/// here and then ignored by the ledger fold (§3.3, AC7); the system assigns
/// the real ID in issue order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewFindingDto {
    pub severity: Severity,
    pub issue: String,
}

/// §3.2/§3.3: the reviewer's reported status for one PRIOR finding ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriorUpdateDto {
    /// The finding ID as the model wrote it (e.g. `"F3"`). Looked up against
    /// system-assigned IDs; an ID that does not match any known finding is
    /// invalid input to the ledger (§8.2: "an illegal state transition").
    pub id: String,
    pub status: FindingStatus,
    /// Required by §3.2 "when declining a human_note" and useful generally;
    /// not otherwise validated as mandatory by this schema (§3.2 does not
    /// require a reason on every status, only make withdrawal/insistence
    /// explainable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// §3.4: the main agent's answer for one open finding, by system-assigned ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RebuttalResponseDto {
    pub id: String,
    pub answer: RebuttalAnswer,
    /// Required for `reject`/`partial` (§3.4); enforced by the validator in
    /// `verdict.rs`, not by this schema type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// §4: "Turn-ending events ... also carry cumulative execution time and
/// cumulative cost-so-far". Factored into its own struct so every
/// turn-ending variant below carries exactly the same two fields.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Cumulative {
    pub exec_time_ms: u64,
    pub cost_usd_micros: u64,
}

/// One logical review event (§4), the payload half of a `Note` event.
/// `#[serde(tag = "type")]` makes the wire shape `{"type": "verdict", ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReviewPayload {
    SessionStarted {
        members: [String; 2],
        mode: Mode,
    },
    TurnSent {
        round: u32,
        to: Role,
        /// Present only for the restarted round N+1 (§8.2); carries
        /// [`super::constants::REVIEW_ROUND_RESTART_NOTE`] verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restart_note: Option<String>,
    },
    /// §4 lists `verdict` as its own event, separate from `finding_issued`/
    /// `finding_status`: the driver follows one `Verdict` event with zero or
    /// more `FindingIssued` (new findings, system-assigned IDs written one
    /// at a time — AC7) and `FindingStatus` events (prior-finding updates)
    /// for the SAME round. The ledger fold treats all of them as one
    /// logical turn tagged by `round`.
    Verdict {
        round: u32,
        kind: VerdictKind,
        #[serde(flatten)]
        cumulative: Cumulative,
    },
    /// One new finding, ID assigned by the SYSTEM (never the model) in issue
    /// order (§3.3, AC7). Whatever the model itself may have called this
    /// finding is not part of this event at all — [`NewFindingDto`] carries
    /// no id field, so there is nothing for the system to defer to.
    FindingIssued {
        round: u32,
        /// System-assigned, `F<n>`.
        id: String,
        severity: Severity,
        issue: String,
    },
    /// The reviewer's reported status for one PRIOR finding (§3.3). A
    /// status for an ID never issued is an illegal transition (§8.2 damage,
    /// checked by the replay fold — see `rollback.rs`).
    FindingStatus {
        round: u32,
        id: String,
        status: FindingStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Rebuttal {
        round: u32,
        responses: Vec<RebuttalResponseDto>,
        #[serde(flatten)]
        cumulative: Cumulative,
    },
    HumanNote {
        text: String,
        /// `None` = broadcast to both sides (§6 plain text). `Some(role)` =
        /// `@<agent>` targeted note.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<Role>,
    },
    Ruling {
        text: String,
        /// Finding IDs this ruling closes (§6 `/rule`).
        closes: Vec<String>,
    },
    Escalation {
        /// The finding ID whose second rejection triggered this (§3.4,
        /// AC8), or empty when escalation came from elsewhere (e.g. the
        /// human forcing a ruling-to-escalation path is out of scope here).
        finding_id: String,
        reason: String,
    },
    Paused {
        reason: String,
        #[serde(flatten)]
        cumulative: Cumulative,
    },
    Resumed {
        #[serde(flatten)]
        cumulative: Cumulative,
    },
    ModeChanged {
        mode: Mode,
    },
    SessionStopped {
        reason: String,
        #[serde(default)]
        unresolved: Vec<String>,
        #[serde(flatten)]
        cumulative: Cumulative,
    },
    /// §8.2 Continue path.
    ResumedFromCheckpoint {
        /// The round resumed FROM (the last complete round).
        round: u32,
        /// 1-based line number of the damage that triggered rollback, when
        /// known (a bad-signature event still has a line number; see
        /// `rollback.rs`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        damage_line: Option<usize>,
        damage_reason: String,
        #[serde(flatten)]
        cumulative: Cumulative,
    },
}

/// The envelope actually stored at `Note.payload.review` (§4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewEnvelope {
    pub v: u32,
    #[serde(flatten)]
    pub payload: ReviewPayload,
}

/// Wrap a payload into the `Note.payload` JSON value this module writes.
pub fn to_note_payload(payload: &ReviewPayload) -> serde_json::Value {
    let envelope = ReviewEnvelope {
        v: REVIEW_SCHEMA_VERSION,
        payload: payload.clone(),
    };
    serde_json::json!({ REVIEW_PAYLOAD_KEY: envelope })
}

/// What a `Note`'s payload is, for the review fold's purposes.
#[derive(Debug)]
pub enum NoteClassification {
    /// No `"review"` key at all — an ordinary note, ignored (§4).
    NotReview,
    /// Has the key and parses.
    Review(ReviewEnvelope),
    /// Has the key but fails to parse as [`ReviewEnvelope`] — damage (§8.2).
    Malformed,
}

/// Classify a `Note`'s raw JSON payload per §4's rule.
pub fn classify_note_payload(payload: &serde_json::Value) -> NoteClassification {
    let Some(review) = payload.get(REVIEW_PAYLOAD_KEY) else {
        return NoteClassification::NotReview;
    };
    match serde_json::from_value::<ReviewEnvelope>(review.clone()) {
        Ok(env) => NoteClassification::Review(env),
        Err(_) => NoteClassification::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_note_payload() {
        let payload = ReviewPayload::Verdict {
            round: 1,
            kind: VerdictKind::Revise,
            cumulative: Cumulative {
                exec_time_ms: 1000,
                cost_usd_micros: 500,
            },
        };
        let note = to_note_payload(&payload);
        match classify_note_payload(&note) {
            NoteClassification::Review(env) => {
                assert_eq!(env.v, REVIEW_SCHEMA_VERSION);
                assert_eq!(env.payload, payload);
            }
            other => panic!("expected Review, got {other:?}"),
        }
    }

    #[test]
    fn a_plain_note_without_the_review_key_is_not_review() {
        let payload = serde_json::json!({"text": "hi"});
        assert!(matches!(
            classify_note_payload(&payload),
            NoteClassification::NotReview
        ));
    }

    #[test]
    fn a_review_key_that_fails_to_parse_is_malformed() {
        let payload = serde_json::json!({"review": {"v": 1, "type": "not_a_real_kind"}});
        assert!(matches!(
            classify_note_payload(&payload),
            NoteClassification::Malformed
        ));
    }

    #[test]
    fn model_supplied_finding_ids_are_not_part_of_the_schema() {
        // NewFindingDto has no `id` field at all — the model cannot supply
        // one through this type, which is the schema-level half of AC7.
        let json = serde_json::json!({"severity": "low", "issue": "x", "id": "F99"});
        let dto: NewFindingDto = serde_json::from_value(json).unwrap();
        assert_eq!(dto.issue, "x");
    }
}
