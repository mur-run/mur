//! Boot steps 2–4 and 8–10 of the startup override (spec
//! `2026-10-25-startup-permission-overrides-design.md` §Runtime boot sequence).
//!
//! Nothing here ever fails the boot: every problem becomes "apply nothing"
//! plus an `override_rejected` record and an audit line.

use std::path::Path;

use chrono::{DateTime, Utc};
use mur_common::agent::{OverrideRecord, OverrideRejection};
use mur_common::permissions::{AuditEvent, GrantStore};
use mur_common::startup_override::claim::{self, ClaimAs, Claimed};
use tracing::{info, warn};

/// Spec step 8 literal: the seal did not establish enforcement.
pub(super) const REASON_UNSEALED: &str = "unsealed";
/// Followed by the I/O error, e.g. `override rejected: claim failed: Permission denied`.
pub(super) const REASON_CLAIM_FAILED_PREFIX: &str = "override rejected: claim failed: ";
/// Followed by the I/O error from reading the claimed file.
const REASON_READ_FAILED_PREFIX: &str = "override rejected: read failed: ";

/// What step 4 decided, carried across the seal to step 8.
#[derive(Debug)]
pub(super) enum Pending {
    None,
    Accepted(OverrideRecord),
    Rejected(OverrideRejection),
}

/// Boot steps 2–4. Call before `pin::verify`, so a boot that is later refused
/// has still consumed the file and cannot leave it for the next boot.
///
/// The live name is never opened: it is renamed to `.consumed.json` first and
/// only that copy is read.
pub(super) fn consume_at_boot(
    mur_home: &Path,
    agent_home: &Path,
    agent: &str,
    stop_timeout_secs: u64,
    now: DateTime<Utc>,
) -> Pending {
    let dir = claim::overrides_dir(mur_home);
    let taken = match claim::claim(&dir, agent, ClaimAs::Consume) {
        Claimed::Took(path) => path,
        Claimed::Absent => {
            if claim::consumed_path(&dir, agent).exists() {
                info!("override not reapplied (one-shot)");
                audit(
                    agent_home,
                    &AuditEvent::StartupOverrideNotReapplied { ts: now },
                );
            }
            return Pending::None;
        }
        Claimed::Failed(e) => {
            warn!(error = %e, "could not claim the startup override; applying nothing");
            return Pending::Rejected(OverrideRejection {
                nonce: None,
                reason: format!("{REASON_CLAIM_FAILED_PREFIX}{e}"),
            });
        }
    };
    let bytes = match std::fs::read(&taken) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "could not read the claimed startup override; applying nothing");
            return Pending::Rejected(OverrideRejection {
                nonce: None,
                reason: format!("{REASON_READ_FAILED_PREFIX}{e}"),
            });
        }
    };
    match mur_common::startup_override::validate(&bytes, now, stop_timeout_secs) {
        Ok(record) => Pending::Accepted(record),
        Err(rejection) => {
            warn!(reason = %rejection.reason, "startup override rejected; applying nothing");
            Pending::Rejected(rejection)
        }
    }
}

/// Boot steps 8–10. An accepted override on a boot whose seal is not
/// enforcing is reported as rejected (`unsealed`), never as active. Writes
/// one audit line per claimed file and returns the two `LockFile` fields.
pub(super) fn finish_after_seal(
    pending: Pending,
    enforcing: bool,
    agent_home: &Path,
) -> (Option<OverrideRecord>, Option<OverrideRejection>) {
    let ts = Utc::now();
    let pending = match pending {
        Pending::Accepted(record) if !enforcing => Pending::Rejected(OverrideRejection {
            nonce: Some(record.nonce),
            reason: REASON_UNSEALED.to_string(),
        }),
        other => other,
    };
    match pending {
        Pending::None => (None, None),
        Pending::Accepted(record) => {
            warn!(
                nonce = %record.nonce,
                flags = ?record.flags,
                confirmed_by = ?record.confirmed_by,
                "startup override applied"
            );
            audit(
                agent_home,
                &AuditEvent::StartupOverrideApplied {
                    ts,
                    nonce: record.nonce.clone(),
                    flags: record.flags,
                    confirmed_by: record.confirmed_by,
                },
            );
            (Some(record), None)
        }
        Pending::Rejected(rejection) => {
            audit(
                agent_home,
                &AuditEvent::StartupOverrideRejected {
                    ts,
                    nonce: rejection.nonce.clone(),
                    reason: rejection.reason.clone(),
                },
            );
            (None, Some(rejection))
        }
    }
}

/// Best-effort: an audit write failure is logged, never fatal to the boot.
fn audit(agent_home: &Path, event: &AuditEvent) {
    if let Err(e) = GrantStore::new(agent_home).append_audit(event) {
        warn!(error = %e, "could not write the startup-override audit line");
    }
}
