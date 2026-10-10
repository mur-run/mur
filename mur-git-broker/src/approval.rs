//! Approval acceptance and consume-once execution (design §4).
//!
//! The broker never judges *who* approved: it receives an `ApprovalProof` that the daemon has
//! already verified against the HITL authority. It only checks that the proof is for exactly
//! this pending row, uses it once, and gives it a short execution window.

use crate::{
    constants::APPROVAL_EXEC_WINDOW_SECS,
    error::BrokerError,
    pending::{PendingStore, RequestKey, State},
};
use chrono::{DateTime, Duration, Utc};

/// The trusted time source, injected so tests control it.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// An approval the daemon has already verified. `event_id` is the tombstone key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalProof {
    pub event_id: String,
    pub request_id: String,
    pub action_hash: String,
}

/// `pending_approval → approved`. Sets `accepted_at` and tombstones the event. Anything that is
/// not an exact, first-time match for a pending row is `NotPending` and changes nothing.
pub fn accept_approval(
    store: &PendingStore,
    key: &RequestKey,
    proof: &ApprovalProof,
    clock: &dyn Clock,
) -> Result<(), BrokerError> {
    store.accept(
        key,
        &proof.event_id,
        &proof.request_id,
        &proof.action_hash,
        clock.now(),
    )
}

/// `approved → executing`, inside the window that started at `accepted_at`. Entering `executing`
/// is what consumes the approval; a late call ends the request as `approval_expired`.
pub fn begin_execution(
    store: &PendingStore,
    key: &RequestKey,
    clock: &dyn Clock,
) -> Result<(), BrokerError> {
    let row = store.get(key)?.ok_or(BrokerError::NotPending)?;
    if row.state != State::Approved {
        return Err(BrokerError::NotPending);
    }
    let accepted = row
        .accepted_at
        .ok_or_else(|| BrokerError::Storage("approved row without accepted_at".into()))?;
    if clock.now() > accepted + Duration::seconds(APPROVAL_EXEC_WINDOW_SECS) {
        store.transition(key, State::Approved, State::ApprovalExpired)?;
        return Err(BrokerError::ApprovalExpired);
    }
    store.transition(key, State::Approved, State::Executing)
}
