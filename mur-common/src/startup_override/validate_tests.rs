use super::*;
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{Value, json};

const STOP: u64 = 15;
const NONCE: &str = "ovr-0123456789abcdef0123456789abcdef";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap()
}

/// A valid, all-false file created `age_secs` before `now()` (negative = future).
fn file(age_secs: i64) -> Value {
    json!({
        "version": OVERRIDE_SCHEMA_VERSION,
        "nonce": NONCE,
        "created_at": (now() - Duration::seconds(age_secs)).to_rfc3339(),
        "issued_by": "cli",
        "confirmed_by": "tty",
        "no_hitl": false,
        "net_unrestricted": false,
        "full_perms": false,
    })
}

fn check(v: &Value) -> Result<OverrideRecord, OverrideRejection> {
    validate(serde_json::to_vec(v).unwrap().as_slice(), now(), STOP)
}

fn rejected(v: &Value) -> OverrideRejection {
    check(v).expect_err("override must be rejected")
}

#[test]
fn all_false_file_is_accepted_with_empty_flags() {
    let rec = check(&file(0)).unwrap();
    assert_eq!(rec.nonce, NONCE);
    assert_eq!(rec.flags, OverrideFlags::default());
    assert_eq!(rec.confirmed_by, ConfirmedBy::Tty);
}

#[test]
fn rejects_stale() {
    let r = rejected(&file(681));
    assert_eq!(r.reason, REASON_STALE);
    assert_eq!(r.nonce.as_deref(), Some(NONCE));
}

#[test]
fn accepts_600s_old_after_a_kick() {
    assert!(check(&file(600)).is_ok());
    assert!(check(&file(680)).is_ok(), "the boundary itself is in range");
}

#[test]
fn rejects_future_beyond_margin() {
    assert_eq!(rejected(&file(-61)).reason, REASON_FUTURE);
}

#[test]
fn accepts_future_within_margin() {
    assert!(check(&file(-59)).is_ok());
}

#[test]
fn rejects_unknown_key_rule4() {
    let mut v = file(0);
    v["cooldown"] = json!(false);
    let r = rejected(&v);
    assert_eq!(r.reason, REASON_UNKNOWN_FIELD);
    assert_eq!(r.nonce.as_deref(), Some(NONCE), "nonce recovered leniently");
}

#[test]
fn rejects_unknown_key_rule7() {
    let mut v = file(0);
    v["secret_scan"] = json!(false);
    assert_eq!(rejected(&v).reason, REASON_UNKNOWN_FIELD);
}

#[test]
fn rejects_bad_version() {
    let mut v = file(0);
    v["version"] = json!(2);
    assert_eq!(rejected(&v).reason, REASON_UNSUPPORTED_VERSION);
}

#[test]
fn rejects_invalid_pair() {
    let mut v = file(0);
    v["issued_by"] = json!("hub");
    assert_eq!(rejected(&v).reason, REASON_INVALID_PAIR);
}

#[test]
fn rejects_unknown_confirmed_by() {
    let mut v = file(0);
    v["confirmed_by"] = json!("mouse");
    assert_eq!(rejected(&v).reason, REASON_UNKNOWN_CONFIRMED_BY);
}

#[test]
fn rejects_unknown_issued_by() {
    let mut v = file(0);
    v["issued_by"] = json!("daemon");
    assert_eq!(rejected(&v).reason, REASON_UNKNOWN_ISSUED_BY);
}

#[test]
fn rejects_malformed_nonce() {
    for bad in [
        "ovr-xyz",
        "ovr-0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcdef",
    ] {
        let mut v = file(0);
        v["nonce"] = json!(bad);
        let r = rejected(&v);
        assert_eq!(r.reason, REASON_MALFORMED_NONCE, "{bad}");
        assert_eq!(r.nonce, None, "an untrusted nonce is never echoed");
    }
}

/// Every flag the schema has is applied by this build; the unsupported-flag
/// check guards a field added later, before a build knows how to apply it.
#[test]
fn this_build_supports_every_flag_in_the_schema() {
    assert_eq!(
        SUPPORTED,
        OverrideFlags {
            no_hitl: true,
            net_unrestricted: true,
            full_perms: true,
        }
    );
}

#[test]
fn accepts_net_unrestricted_and_full_perms() {
    let mut v = file(0);
    v["net_unrestricted"] = json!(true);
    v["full_perms"] = json!(true);
    let r = check(&v).expect("both are supported");
    assert!(r.flags.net_unrestricted && r.flags.full_perms);
    assert!(!r.flags.no_hitl);
}

#[test]
fn accepts_no_hitl() {
    let mut v = file(0);
    v["no_hitl"] = json!(true);
    let r = check(&v).expect("no_hitl is supported");
    assert!(r.flags.no_hitl);
    assert!(!r.flags.net_unrestricted && !r.flags.full_perms);
}

#[test]
fn unparseable_bytes_reject_without_a_nonce() {
    let r = validate(b"{not json", now(), STOP).unwrap_err();
    assert_eq!(r.reason, REASON_UNPARSEABLE);
    assert_eq!(r.nonce, None);
}

#[test]
fn missing_field_is_unparseable() {
    let mut v = file(0);
    v.as_object_mut().unwrap().remove("full_perms");
    assert_eq!(rejected(&v).reason, REASON_UNPARSEABLE);
}

#[test]
fn version_is_checked_before_age() {
    let mut v = file(10_000);
    v["version"] = json!(2);
    assert_eq!(rejected(&v).reason, REASON_UNSUPPORTED_VERSION);
}

#[test]
fn file_round_trips_its_flags() {
    let mut v = file(0);
    v["net_unrestricted"] = json!(true);
    let f: OverrideFile = serde_json::from_value(v).unwrap();
    assert_eq!(
        f.flags(),
        OverrideFlags {
            net_unrestricted: true,
            ..OverrideFlags::default()
        }
    );
}
