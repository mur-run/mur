//! Regression: `rollback_profile` rewrites `profile.yaml`, so it must move the
//! entitlement pin with it. Before the fix the pin stayed at the newer version
//! and the supervisor's pin check (the same `entitlements_pin::check` that
//! `supervisor/pin.rs` calls) refused the next start with
//! `entitlements_unpinned`.
//!
//! The pin only moves from a trusted state, same as every other trusted
//! writer: a profile tampered with before the rollback keeps the pin where it
//! was, so the rollback cannot launder the tampering.

use mur_common::entitlements_pin::{self, PinCheck};
use mur_core::store::versioned::agent::VersionedAgentStore;

const AGENT: &str = "bridge_test";
const V1: &str = include_str!("../../mur-common/tests/fixtures/minimal_profile.yaml");

fn ent(yaml: &str) -> mur_common::agent::Entitlements {
    entitlements_pin::entitlements_from_yaml(yaml, std::path::Path::new("profile.yaml")).unwrap()
}

fn on_disk(mur_home: &std::path::Path) -> String {
    std::fs::read_to_string(mur_home.join("agents").join(AGENT).join("profile.yaml")).unwrap()
}

fn pin_check(mur_home: &std::path::Path) -> PinCheck {
    entitlements_pin::check(mur_home, AGENT, &ent(&on_disk(mur_home))).unwrap()
}

/// v1 pinned, v2 widens fs write and advances the pin (an approved grant).
fn granted_store(mur_home: &std::path::Path) -> (VersionedAgentStore, String) {
    let mut store = VersionedAgentStore::init(&mur_home.join("agents")).unwrap();
    store.save_profile(AGENT, V1, "init").unwrap();
    entitlements_pin::write_pin(mur_home, AGENT, &ent(V1)).unwrap();

    let v2 = V1.replace("write: []", "write: [\"/tmp/granted\"]");
    assert_ne!(v2, V1, "fixture must contain `write: []`");
    store.save_profile(AGENT, &v2, "grant fs write").unwrap();
    assert!(entitlements_pin::advance_pin(mur_home, AGENT, Some(&ent(V1)), &ent(&v2)).unwrap());
    assert_eq!(
        pin_check(mur_home),
        PinCheck::Match,
        "baseline: after a pinned grant the agent would start"
    );
    (store, v2)
}

#[test]
fn rollback_of_a_granted_entitlement_moves_the_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let (mut store, _) = granted_store(mur_home);

    store.rollback_profile(AGENT, 1).unwrap();
    assert!(
        !on_disk(mur_home).contains("/tmp/granted"),
        "rollback restored v1 on disk"
    );
    assert_eq!(
        pin_check(mur_home),
        PinCheck::Match,
        "the agent must still start after a rollback"
    );
}

#[test]
fn rollback_over_a_tampered_profile_keeps_the_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let (mut store, v2) = granted_store(mur_home);

    // Widened outside MUR after the pinned grant.
    let tampered = v2.replace("/tmp/granted", "/");
    std::fs::write(
        mur_home.join("agents").join(AGENT).join("profile.yaml"),
        &tampered,
    )
    .unwrap();

    store.rollback_profile(AGENT, 1).unwrap();
    assert_eq!(
        entitlements_pin::check(mur_home, AGENT, &ent(&v2)).unwrap(),
        PinCheck::Match,
        "the pin must not move from a tampered state"
    );
}
