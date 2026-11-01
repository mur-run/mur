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

/// SSH proxy spec: `entitlements.ssh` is protected ONLY because the pin covers
/// it. A profile edit that touches nothing but the ssh block must be a mismatch.
#[test]
fn ssh_only_edit_in_profile_yaml_is_a_pin_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let base = serde_yaml_ng::to_string(&AgentProfile::default_for_tests()).unwrap();
    let mut v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&base).unwrap();
    v["entitlements"]["ssh"] = serde_yaml_ng::from_str(
        "keys:\n  - fingerprint: \"SHA256:attacker\"\n    allow_hosts:\n      - host_key: \"SHA256:anyhost\"\n",
    )
    .unwrap();
    let tampered = serde_yaml_ng::to_string(&v).unwrap();
    let p = Path::new("profile.yaml");

    write_pin(tmp.path(), "a", &entitlements_from_yaml(&base, p).unwrap()).unwrap();
    let edited = entitlements_from_yaml(&tampered, p).unwrap();
    assert_eq!(
        check(tmp.path(), "a", &edited).unwrap(),
        PinCheck::Mismatch {
            changed: vec!["ssh".to_string()]
        }
    );
}

#[test]
fn changed_entitlements_names_ssh_when_only_ssh_differs() {
    let mut edited = ent();
    edited.ssh.keys.push(crate::agent::SshKeyRule {
        fingerprint: "SHA256:attacker".to_string(),
        label: None,
        allow_hosts: Vec::new(),
    });
    assert_eq!(
        changed_entitlements(&ent(), &edited),
        vec!["ssh".to_string()]
    );
}

#[test]
fn empty_ssh_block_is_not_serialized_so_existing_pins_still_match() {
    let v = serde_json::to_value(ent()).unwrap();
    assert!(
        v.get("ssh").is_none(),
        "empty ssh must not appear in pin JSON"
    );
}

/// A pin written by the build that stored one `known_host` per host rule must
/// still match the same profile once `known_hosts` replaces it: both sides
/// round-trip through the current schema, so the agent keeps starting.
#[test]
fn a_pin_with_the_old_single_known_host_still_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let base = serde_yaml_ng::to_string(&AgentProfile::default_for_tests()).unwrap();
    let mut v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&base).unwrap();
    v["entitlements"]["ssh"] = serde_yaml_ng::from_str(
        "keys:\n  - fingerprint: \"SHA256:k\"\n    allow_hosts:\n      - host_key: \"SHA256:h\"\n        known_host:\n          host: github.com\n          key_type: ssh-ed25519\n          key_base64: AAAA\n",
    )
    .unwrap();
    let old_yaml = serde_yaml_ng::to_string(&v).unwrap();
    // The pin file as the old build wrote it: its JSON carries `known_host`.
    let mut pin_json = serde_json::to_value(ent()).unwrap();
    pin_json["ssh"] = serde_json::to_value(&v["entitlements"]["ssh"]).unwrap();
    let path = pin_path(tmp.path(), "a");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = serde_json::json!({ "version": PIN_VERSION, "entitlements": pin_json });
    std::fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();

    let profile = entitlements_from_yaml(&old_yaml, Path::new("profile.yaml")).unwrap();
    assert_eq!(profile.ssh.keys[0].allow_hosts[0].known_hosts.len(), 1);
    assert_eq!(check(tmp.path(), "a", &profile).unwrap(), PinCheck::Match);
}
