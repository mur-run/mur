use super::*;
use mur_common::agent::{LockTransports, OverrideFlags, OverrideRecord, OverrideRejection};
use mur_common::startup_override::ConfirmedBy;

const NONCE: &str = "ovr-0123456789abcdef0123456789abcdef";

fn lock() -> LockFile {
    LockFile {
        schema: 1,
        uuid: "u".into(),
        name: "alpha".into(),
        pid: 1,
        ppid: 0,
        started_at: "t".into(),
        binary_version: "v".into(),
        transports: LockTransports {
            stdio: true,
            unix_socket: None,
            tcp: None,
            webhook: None,
        },
        card_digest: "d".into(),
        capabilities: vec![],
        build_sha: String::new(),
        proto_version: 0,
        sandbox: None,
        startup_override: None,
        override_rejected: None,
    }
}

fn active() -> LockFile {
    LockFile {
        startup_override: Some(OverrideRecord {
            nonce: NONCE.into(),
            flags: OverrideFlags {
                no_hitl: true,
                net_unrestricted: true,
                full_perms: false,
            },
            confirmed_by: ConfirmedBy::Tty,
        }),
        ..lock()
    }
}

#[test]
fn no_override_prints_nothing() {
    assert!(lines(&lock(), true, Some(0)).is_empty());
}

#[test]
fn an_active_override_names_its_nonce_and_flags() {
    assert_eq!(
        lines(&active(), true, Some(0)),
        vec![format!(
            "     ⚠ override active ({NONCE}): --no-hitl, --net=unrestricted"
        )]
    );
}

#[test]
fn a_dead_pid_hides_the_override() {
    assert!(lines(&active(), false, Some(3)).is_empty());
}

#[test]
fn waiting_calls_are_counted_and_point_at_the_command() {
    let out = lines(&active(), true, Some(2));
    assert_eq!(out.len(), 2);
    assert_eq!(
        out[1],
        "       2 call(s) waiting for a human — `mur agent hitl alpha pending`"
    );
}

#[test]
fn an_unanswered_count_says_so_rather_than_zero() {
    let out = lines(&active(), true, None);
    assert!(out[1].contains("unknown"), "{out:?}");
}

#[test]
fn a_rejected_override_states_the_reason() {
    let l = LockFile {
        override_rejected: Some(OverrideRejection {
            nonce: Some(NONCE.into()),
            reason: "unsealed".into(),
        }),
        ..lock()
    };
    assert_eq!(
        lines(&l, true, None),
        vec![format!(
            "     override not applied ({NONCE}): unsealed — this boot runs with the profile as written"
        )]
    );
}
