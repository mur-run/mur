//! What a boot did with our override, and what the orchestrator reports.
//! One `classify`, used by the acknowledgement wait and by Cleanup's
//! `ENOENT` arm alike, so a late answer reads exactly like an on-time one.
//!
//! Spec: `2026-10-25-startup-overrides-hub-design.md` §Outcome.

use mur_common::LockFile;
use mur_common::agent::OverrideFlags;
use serde::Serialize;

/// The lock file's answer for one nonce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ack {
    Applied(OverrideFlags),
    Rejected(String),
    /// Neither field names our nonce: not this boot's answer (yet).
    Other,
}

pub(crate) fn classify(lock: &LockFile, nonce: &str) -> Ack {
    if let Some(rec) = &lock.startup_override
        && rec.nonce == nonce
    {
        return Ack::Applied(rec.flags);
    }
    if let Some(rej) = &lock.override_rejected
        && rej.nonce.as_deref() == Some(nonce)
    {
        return Ack::Rejected(rej.reason.clone());
    }
    Ack::Other
}

/// Where the orchestrator stopped. `Refusal` covers every check before the
/// writer lock is held or anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    Refusal {
        reason: String,
    },
    Sweep,
    /// The job gate refused the publish (Hub quit); the CLI never hits it.
    Cancelled,
    Publish,
    Stop,
    Start,
    NoAck,
}

/// What Cleanup did after a failure past publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Cleanup {
    /// W: our file was taken back; no boot saw it.
    Withdrawn,
    /// C: a boot consumed it but recorded neither applied nor rejected.
    ConsumedUnacknowledged,
    /// F: the withdraw failed; a live file remains.
    WithdrawFailed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverrideOutcome {
    /// `late`: read in Cleanup's `ENOENT` arm. `detail`: the failed step's
    /// error that Cleanup's late read overrode.
    Applied {
        flags: OverrideFlags,
        late: bool,
        detail: Option<String>,
    },
    Rejected {
        reason: String,
        late: bool,
        detail: Option<String>,
    },
    Failed {
        step: Step,
        cleanup: Option<Cleanup>,
        detail: String,
    },
    /// Hub quit took the claim; never produced by the CLI.
    Abandoned { reason: String },
}

impl OverrideOutcome {
    /// The CLI exit code follows this: only an applied override is success.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
