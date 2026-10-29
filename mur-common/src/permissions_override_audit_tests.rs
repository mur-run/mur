//! Writer-side startup-override audit lines: the `type` strings and field
//! names are a file format, so they are pinned here.

use super::AuditEvent;
use crate::agent::OverrideFlags;
use crate::startup_override::{ConfirmedBy, IssuedBy};
use serde_json::{Value, json};

const NONCE: &str = "ovr-0123456789abcdef0123456789abcdef";

fn ts() -> chrono::DateTime<chrono::Utc> {
    "2026-10-28T12:00:00Z".parse().unwrap()
}

fn flags() -> OverrideFlags {
    OverrideFlags {
        no_hitl: true,
        net_unrestricted: false,
        full_perms: true,
    }
}

fn round_trip(ev: &AuditEvent) -> Value {
    let v = serde_json::to_value(ev).unwrap();
    let back: AuditEvent = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), v);
    v
}

#[test]
fn issued_line_is_pinned_and_omits_a_cli_window() {
    let v = round_trip(&AuditEvent::StartupOverrideIssued {
        ts: ts(),
        nonce: NONCE.into(),
        flags: flags(),
        issued_by: IssuedBy::Cli,
        confirmed_by: ConfirmedBy::FlagConfirm,
        window: None,
    });
    assert_eq!(
        v,
        json!({
            "type": "startup_override_issued",
            "ts": "2026-10-28T12:00:00Z",
            "nonce": NONCE,
            "flags": {"no_hitl": true, "net_unrestricted": false, "full_perms": true},
            "issued_by": "cli",
            "confirmed_by": "flag:--confirm",
        })
    );
}

#[test]
fn hub_window_is_written_when_present() {
    let v = round_trip(&AuditEvent::StartupOverrideDeclined {
        ts: ts(),
        flags: flags(),
        window: Some("main".into()),
    });
    assert_eq!(v["type"], "startup_override_declined");
    assert_eq!(v["window"], "main");
}

#[test]
fn every_writer_variant_has_its_type_string() {
    let cases = [
        (
            AuditEvent::StartupOverrideDeclined {
                ts: ts(),
                flags: flags(),
                window: None,
            },
            "startup_override_declined",
        ),
        (
            AuditEvent::StartupOverrideDiscarded {
                ts: ts(),
                nonce: None,
                window: None,
            },
            "startup_override_discarded",
        ),
        (
            AuditEvent::StartupOverrideWithdrawn {
                ts: ts(),
                nonce: NONCE.into(),
                reason: "launch failed".into(),
                window: None,
            },
            "startup_override_withdrawn",
        ),
        (
            AuditEvent::StartupOverrideUnacknowledged {
                ts: ts(),
                nonce: NONCE.into(),
                window: None,
            },
            "startup_override_unacknowledged",
        ),
        (
            AuditEvent::StartupOverrideWithdrawFailed {
                ts: ts(),
                nonce: Some(NONCE.into()),
                error: "permission denied".into(),
                window: None,
            },
            "startup_override_withdraw_failed",
        ),
    ];
    for (ev, ty) in cases {
        let v = round_trip(&ev);
        assert_eq!(v["type"], ty);
        assert!(
            v.get("window").is_none(),
            "{ty}: CLI window must be omitted"
        );
    }
}

#[test]
fn discarded_keeps_a_missing_nonce_as_null() {
    let v = round_trip(&AuditEvent::StartupOverrideDiscarded {
        ts: ts(),
        nonce: None,
        window: None,
    });
    assert_eq!(v["nonce"], Value::Null);
}

#[test]
fn hub_refusal_line_is_pinned() {
    let v = round_trip(&AuditEvent::StartupOverrideRefused {
        ts: ts(),
        reason: "dialog open".into(),
        window: Some("dashboard".into()),
    });
    assert_eq!(
        v,
        json!({
            "type": "startup_override_refused",
            "ts": "2026-10-28T12:00:00Z",
            "reason": "dialog open",
            "window": "dashboard",
        })
    );
}

#[test]
fn hub_issued_line_carries_the_native_dialog_label() {
    let v = round_trip(&AuditEvent::StartupOverrideIssued {
        ts: ts(),
        nonce: NONCE.into(),
        flags: flags(),
        issued_by: IssuedBy::Hub,
        confirmed_by: ConfirmedBy::HubNativeDialog,
        window: Some("dashboard".into()),
    });
    assert_eq!(v["issued_by"], "hub");
    assert_eq!(v["confirmed_by"], "hub:native-dialog");
}
