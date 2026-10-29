use super::*;
use mur_common::agent::{OverrideRecord, OverrideRejection};
use mur_common::startup_override::ConfirmedBy;
use serde_json::json;

const OURS: &str = "ovr-0123456789abcdef0123456789abcdef";
const THEIRS: &str = "ovr-ffffffffffffffffffffffffffffffff";

fn lock() -> LockFile {
    serde_json::from_value(json!({
        "schema": 1, "uuid": "u", "name": "alpha", "pid": 42, "ppid": 1,
        "started_at": "2026-10-28T00:00:00Z", "binary_version": "0.0.0",
        "transports": {"stdio": true, "unix_socket": null, "tcp": null},
        "card_digest": "d", "capabilities": [], "build_sha": "s", "proto_version": 1,
    }))
    .unwrap()
}

fn flags() -> OverrideFlags {
    OverrideFlags {
        no_hitl: true,
        ..OverrideFlags::default()
    }
}

fn applied(nonce: &str) -> LockFile {
    let mut l = lock();
    l.startup_override = Some(OverrideRecord {
        nonce: nonce.into(),
        flags: flags(),
        confirmed_by: ConfirmedBy::Tty,
    });
    l
}

fn rejected(nonce: Option<&str>) -> LockFile {
    let mut l = lock();
    l.override_rejected = Some(OverrideRejection {
        nonce: nonce.map(str::to_owned),
        reason: "override rejected: stale".into(),
    });
    l
}

#[test]
fn our_nonce_applied() {
    assert_eq!(classify(&applied(OURS), OURS), Ack::Applied(flags()));
}

#[test]
fn our_nonce_rejected() {
    assert_eq!(
        classify(&rejected(Some(OURS)), OURS),
        Ack::Rejected("override rejected: stale".into())
    );
}

#[test]
fn someone_elses_answer_is_other() {
    assert_eq!(classify(&applied(THEIRS), OURS), Ack::Other);
    assert_eq!(classify(&rejected(Some(THEIRS)), OURS), Ack::Other);
    // A rejection with no trustworthy nonce is never ours.
    assert_eq!(classify(&rejected(None), OURS), Ack::Other);
    assert_eq!(classify(&lock(), OURS), Ack::Other);
}

#[test]
fn only_applied_is_success() {
    let ok = OverrideOutcome::Applied {
        flags: flags(),
        late: true,
        detail: Some("stop failed".into()),
    };
    assert!(ok.is_success());
    let rej = OverrideOutcome::Rejected {
        reason: "x".into(),
        late: false,
        detail: None,
    };
    assert!(!rej.is_success());
}

#[test]
fn outcome_serializes_with_kind_tags() {
    let v = serde_json::to_value(OverrideOutcome::Failed {
        step: Step::NoAck,
        cleanup: Some(Cleanup::WithdrawFailed {
            error: "EACCES".into(),
        }),
        detail: "no acknowledgement".into(),
    })
    .unwrap();
    assert_eq!(
        v,
        json!({
            "kind": "failed",
            "step": {"kind": "no_ack"},
            "cleanup": {"kind": "withdraw_failed", "error": "EACCES"},
            "detail": "no acknowledgement",
        })
    );
}
