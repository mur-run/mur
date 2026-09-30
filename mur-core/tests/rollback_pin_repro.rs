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
const V1_FIXTURE: &str = include_str!("../../mur-common/tests/fixtures/minimal_profile.yaml");

/// The fixture with LF endings. A Windows checkout (`core.autocrlf=true`)
/// gives CRLF, and the agents repo stores and returns LF, so byte-equality
/// against the raw fixture would only test the runner's git config.
fn v1() -> String {
    V1_FIXTURE.replace("\r\n", "\n")
}

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
    store.save_profile(AGENT, &v1(), "init").unwrap();
    entitlements_pin::write_pin(mur_home, AGENT, &ent(&v1())).unwrap();

    let v2 = v1().replace("write: []", "write: [\"/tmp/granted\"]");
    assert_ne!(v2, v1(), "fixture must contain `write: []`");
    store.save_profile(AGENT, &v2, "grant fs write").unwrap();
    assert!(entitlements_pin::advance_pin(mur_home, AGENT, Some(&ent(&v1())), &ent(&v2)).unwrap());
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

#[test]
fn rollback_ignores_a_tampered_archive_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let (mut store, _) = granted_store(mur_home);

    // `archive/` lives in the agent's own (writable) home; the agents git
    // repo does not. A widened archive copy must not become the rollback.
    let archive = mur_home
        .join("agents")
        .join(AGENT)
        .join("archive")
        .join("v1.yaml");
    let widened = std::fs::read_to_string(&archive)
        .unwrap()
        .replace("write: []", "write: [\"/\"]");
    assert!(
        widened.contains("write: [\"/\"]"),
        "fixture must contain `write: []`"
    );
    std::fs::write(&archive, &widened).unwrap();

    store.rollback_profile(AGENT, 1).unwrap();
    assert_eq!(
        on_disk(mur_home),
        v1(),
        "rollback restores committed v1, not the archive copy"
    );
    assert_eq!(
        entitlements_pin::check(mur_home, AGENT, &ent(&v1())).unwrap(),
        PinCheck::Match,
        "the pin follows committed v1"
    );
}

#[test]
fn rollback_reports_changed_entitlements_and_pin_state() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let (mut store, v2) = granted_store(mur_home);

    let out = store.rollback_profile(AGENT, 1).unwrap();
    assert_eq!(out.changed_entitlements, vec!["filesystem".to_string()]);
    assert!(out.pin_advanced, "trusted rollback moves the pin");

    // Rolling back to v2 widens again; the caller must be able to say so.
    let out = store.rollback_profile(AGENT, 2).unwrap();
    assert_eq!(on_disk(mur_home), v2);
    assert_eq!(out.changed_entitlements, vec!["filesystem".to_string()]);

    // Tampered current profile: rollback succeeds, pin stays, caller is told.
    let tampered = v2.replace("/tmp/granted", "/");
    std::fs::write(
        mur_home.join("agents").join(AGENT).join("profile.yaml"),
        &tampered,
    )
    .unwrap();
    let out = store.rollback_profile(AGENT, 1).unwrap();
    assert!(!out.pin_advanced, "tampered prior must not advance the pin");
}
