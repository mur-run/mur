//! Boot steps 2–4, 6 and 8–10 of the startup override (spec
//! `2026-10-25-startup-permission-overrides-design.md` §Runtime boot sequence).
//!
//! Nothing here ever fails the boot: every problem becomes "apply nothing"
//! plus an `override_rejected` record and an audit line.

use std::path::Path;

use crate::lock_file::{LockError, LockHandle};

use chrono::{DateTime, Utc};
use mur_common::agent::{Entitlements, OverrideRecord, OverrideRejection};
use mur_common::hitl::startup_waiver::StartupWaiver;
use mur_common::permissions::{AuditEvent, GrantStore};
use mur_common::startup_override::claim::{self, ClaimAs, Claimed};
use mur_common::startup_override::merge;
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
    /// Already audited by `consume_at_boot`: `pin::refuse` can exit before
    /// `finish_after_seal` runs, so the audit line cannot wait for the seal.
    Rejected(OverrideRejection),
}

/// Take `running.lock`, then — only if it was ours — claim the override.
///
/// Order matters: two boots of one agent can overlap (launchd `KeepAlive`
/// and a direct spawn), and the claim is a rename that only one of them wins.
/// Claiming first let the boot that then lost the lock consume the file and
/// exit `already running` with no audit line, so the surviving boot logged
/// "not reapplied (one-shot)" for an override nobody applied. The claim
/// cannot move after the seal instead (the overrides dir is read-only
/// there), so the lock moves before it.
pub(super) fn lock_then_consume(
    lock_path: &Path,
    mur_home: &Path,
    agent_home: &Path,
    agent: &str,
    stop_timeout_secs: u64,
    now: DateTime<Utc>,
) -> Result<(LockHandle, Pending), LockError> {
    let lock = LockHandle::acquire(lock_path)?;
    let pending = consume_at_boot(mur_home, agent_home, agent, stop_timeout_secs, now);
    Ok((lock, pending))
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
            return reject(
                agent_home,
                now,
                OverrideRejection {
                    nonce: None,
                    reason: format!("{REASON_CLAIM_FAILED_PREFIX}{e}"),
                },
            );
        }
    };
    let bytes = match std::fs::read(&taken) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "could not read the claimed startup override; applying nothing");
            return reject(
                agent_home,
                now,
                OverrideRejection {
                    nonce: None,
                    reason: format!("{REASON_READ_FAILED_PREFIX}{e}"),
                },
            );
        }
    };
    match mur_common::startup_override::validate(&bytes, now, stop_timeout_secs) {
        Ok(record) => Pending::Accepted(record),
        Err(rejection) => {
            warn!(reason = %rejection.reason, "startup override rejected; applying nothing");
            reject(agent_home, now, rejection)
        }
    }
}

/// This boot's `--no-hitl` waiver: only for an accepted override that asked
/// for it, and only on a sealed boot — `finish_after_seal` reports an unsealed
/// boot's override as rejected, and what is reported rejected is never applied.
pub(super) fn waiver(pending: &Pending, enforcing: bool) -> Option<StartupWaiver> {
    match pending {
        Pending::Accepted(record) if enforcing => StartupWaiver::from_record(record),
        _ => None,
    }
}

/// Audit a pre-seal rejection now, before `pin::verify` gets a chance to exit.
fn reject(agent_home: &Path, ts: DateTime<Utc>, rejection: OverrideRejection) -> Pending {
    audit_rejected(agent_home, ts, &rejection);
    Pending::Rejected(rejection)
}

/// Boot step 6: fold an accepted override's flags into the in-memory
/// entitlements, after `pin::verify` and before the seal. Returns the
/// entitlements as `profile.yaml` has them, for [`revert_if_unsealed`] and
/// the reload diff base, plus the change summary for the audit (step 10).
/// `None` when nothing was merged.
pub(super) fn merge_into(
    pending: &Pending,
    ent: &mut Entitlements,
) -> Option<(Entitlements, Vec<String>)> {
    let Pending::Accepted(record) = pending else {
        return None;
    };
    let pinned = ent.clone();
    let changes = merge::apply(ent, record.flags);
    for line in &changes {
        warn!(nonce = %record.nonce, "startup override relaxes {line}");
    }
    Some((pinned, changes))
}

/// An unsealed boot reports its override as rejected (`unsealed`, step 8);
/// what is reported rejected is never applied, so the in-memory entitlements
/// go back to the pinned ones before B0, the live profile or the runner read
/// them. Returns whether it reverted.
pub(super) fn revert_if_unsealed(
    merged: &Option<(Entitlements, Vec<String>)>,
    enforcing: bool,
    ent: &mut Entitlements,
) -> bool {
    match merged {
        Some((pinned, _)) if !enforcing && ent != pinned => {
            warn!("seal is not enforcing: startup override relaxation withdrawn");
            *ent = pinned.clone();
            true
        }
        _ => false,
    }
}

/// Boot steps 8–10. An accepted override on a boot whose seal is not
/// enforcing is reported as rejected (`unsealed`), never as active. Together
/// with `consume_at_boot` this writes one audit line per claimed file; it
/// returns the two `LockFile` fields. `changes` is [`merge_into`]'s summary.
pub(super) fn finish_after_seal(
    pending: Pending,
    enforcing: bool,
    changes: Vec<String>,
    agent_home: &Path,
) -> (Option<OverrideRecord>, Option<OverrideRejection>) {
    let ts = Utc::now();
    match pending {
        Pending::None => (None, None),
        Pending::Accepted(record) if !enforcing => {
            let rejection = OverrideRejection {
                nonce: Some(record.nonce),
                reason: REASON_UNSEALED.to_string(),
            };
            audit_rejected(agent_home, ts, &rejection);
            (None, Some(rejection))
        }
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
                    changes,
                },
            );
            (Some(record), None)
        }
        // Audited when it was produced, in `consume_at_boot`.
        Pending::Rejected(rejection) => (None, Some(rejection)),
    }
}

fn audit_rejected(agent_home: &Path, ts: DateTime<Utc>, rejection: &OverrideRejection) {
    audit(
        agent_home,
        &AuditEvent::StartupOverrideRejected {
            ts,
            nonce: rejection.nonce.clone(),
            reason: rejection.reason.clone(),
        },
    );
}

/// Best-effort: an audit write failure is logged, never fatal to the boot.
fn audit(agent_home: &Path, event: &AuditEvent) {
    if let Err(e) = GrantStore::new(agent_home).append_audit(event) {
        warn!(error = %e, "could not write the startup-override audit line");
    }
}
