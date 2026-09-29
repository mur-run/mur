use super::*;
use crate::agent::NetworkOutboundMode;

fn ent() -> Entitlements {
    AgentProfile::default_for_tests().entitlements
}

#[test]
fn missing_pin_reports_missing() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(check(tmp.path(), "a", &ent()).unwrap(), PinCheck::Missing);
}

#[test]
fn round_trip_matches() {
    let tmp = tempfile::tempdir().unwrap();
    write_pin(tmp.path(), "a", &ent()).unwrap();
    assert_eq!(check(tmp.path(), "a", &ent()).unwrap(), PinCheck::Match);
}

#[test]
fn widened_entitlement_is_a_mismatch_naming_the_key() {
    let tmp = tempfile::tempdir().unwrap();
    write_pin(tmp.path(), "a", &ent()).unwrap();
    let mut widened = ent();
    widened.filesystem.write.push("/".to_string());
    widened.network.outbound.mode = NetworkOutboundMode::Unrestricted;
    assert_eq!(
        check(tmp.path(), "a", &widened).unwrap(),
        PinCheck::Mismatch {
            changed: vec!["filesystem".to_string(), "network".to_string()]
        }
    );
}

#[test]
fn pin_missing_a_newer_defaulted_field_still_matches() {
    // A pin written before a defaulted field existed must not brick the agent
    // after an upgrade: drop `tools` and `llm` from the stored JSON.
    let tmp = tempfile::tempdir().unwrap();
    write_pin(tmp.path(), "a", &ent()).unwrap();
    let path = pin_path(tmp.path(), "a");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let e = v["entitlements"].as_object_mut().unwrap();
    e.remove("tools");
    e.remove("llm");
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    assert_eq!(check(tmp.path(), "a", &ent()).unwrap(), PinCheck::Match);
}

#[test]
fn corrupt_pin_is_an_error_not_a_match() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(PINS_DIR)).unwrap();
    std::fs::write(pin_path(tmp.path(), "a"), b"{").unwrap();
    assert!(matches!(
        check(tmp.path(), "a", &ent()),
        Err(PinError::Invalid { .. })
    ));
}

#[test]
fn traversal_name_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        write_pin(tmp.path(), "../x", &ent()),
        Err(PinError::Name(_))
    ));
}

#[test]
fn repin_reads_unexpanded_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agents").join("a");
    std::fs::create_dir_all(&home).unwrap();
    let yaml = include_str!("../../tests/fixtures/minimal_profile.yaml")
        .replace("write: []", "write: [\"{{agent_home}}/out\"]");
    std::fs::write(home.join("profile.yaml"), &yaml).unwrap();
    repin_from_profile(tmp.path(), "a").unwrap();
    let on_disk = entitlements_from_yaml(&yaml, &home).unwrap();
    assert_eq!(check(tmp.path(), "a", &on_disk).unwrap(), PinCheck::Match);
}

#[test]
fn advance_from_trusted_state_moves_the_pin() {
    let tmp = tempfile::tempdir().unwrap();
    write_pin(tmp.path(), "a", &ent()).unwrap();
    let mut granted = ent();
    granted.filesystem.read.push("/data".to_string());
    assert!(advance_pin(tmp.path(), "a", Some(&ent()), &granted).unwrap());
    assert_eq!(check(tmp.path(), "a", &granted).unwrap(), PinCheck::Match);
}

#[test]
fn advance_from_tampered_state_does_not_launder() {
    let tmp = tempfile::tempdir().unwrap();
    write_pin(tmp.path(), "a", &ent()).unwrap();
    let mut tampered = ent();
    tampered.filesystem.write.push("/".to_string());
    // An unrelated CLI save re-writes the tampered entitlements unchanged.
    assert!(!advance_pin(tmp.path(), "a", Some(&tampered), &tampered).unwrap());
    assert!(matches!(
        check(tmp.path(), "a", &tampered).unwrap(),
        PinCheck::Mismatch { .. }
    ));
}

#[test]
fn advance_for_a_new_profile_pins() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(advance_pin(tmp.path(), "a", None, &ent()).unwrap());
    assert_eq!(check(tmp.path(), "a", &ent()).unwrap(), PinCheck::Match);
}

#[test]
fn rename_carries_and_remove_clears() {
    let tmp = tempfile::tempdir().unwrap();
    write_pin(tmp.path(), "a", &ent()).unwrap();
    rename_pin(tmp.path(), "a", "b").unwrap();
    assert_eq!(check(tmp.path(), "a", &ent()).unwrap(), PinCheck::Missing);
    assert_eq!(check(tmp.path(), "b", &ent()).unwrap(), PinCheck::Match);
    remove_pin(tmp.path(), "b").unwrap();
    remove_pin(tmp.path(), "b").unwrap();
    rename_pin(tmp.path(), "b", "c").unwrap();
    assert_eq!(check(tmp.path(), "b", &ent()).unwrap(), PinCheck::Missing);
}
