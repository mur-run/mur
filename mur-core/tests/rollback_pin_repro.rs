//! Reproduces: `rollback_profile` rewrites `profile.yaml` but not the
//! entitlement pin, so the supervisor's pin check (the same
//! `entitlements_pin::check` that `supervisor/pin.rs` calls) no longer matches.
//!
//! Characterization test for the perm-elevation design (Q9): a revert must NOT
//! reuse `rollback_profile` as-is. This test is green while the bug exists.
//! When rollback learns to re-pin, it will fail: invert it to assert
//! `PinCheck::Match` after rollback (keep it as the regression test, don't
//! delete it) and update the perm-elevation design notes.

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

#[test]
fn rollback_of_a_granted_entitlement_leaves_pin_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let mut store = VersionedAgentStore::init(&mur_home.join("agents")).unwrap();

    // v1: trusted install, pinned.
    store.save_profile(AGENT, V1, "init").unwrap();
    entitlements_pin::write_pin(mur_home, AGENT, &ent(V1)).unwrap();

    // v2: a trusted grant widens fs write and advances the pin (what an
    // approved elevation would do).
    let v2 = V1.replace("write: []", "write: [\"/tmp/granted\"]");
    assert_ne!(v2, V1, "fixture must contain `write: []`");
    store.save_profile(AGENT, &v2, "grant fs write").unwrap();
    assert!(entitlements_pin::advance_pin(mur_home, AGENT, Some(&ent(V1)), &ent(&v2)).unwrap());
    assert_eq!(
        entitlements_pin::check(mur_home, AGENT, &ent(&on_disk(mur_home))).unwrap(),
        PinCheck::Match,
        "baseline: after a pinned grant the agent would start"
    );

    // Revert via the existing rollback.
    store.rollback_profile(AGENT, 1).unwrap();
    assert!(
        !on_disk(mur_home).contains("/tmp/granted"),
        "rollback restored v1 on disk"
    );

    // The pin still holds v2, so the supervisor would refuse with
    // `entitlements_unpinned` on the next start.
    match entitlements_pin::check(mur_home, AGENT, &ent(&on_disk(mur_home))).unwrap() {
        PinCheck::Mismatch { changed } => {
            assert_eq!(changed, vec!["filesystem".to_string()]);
        }
        other => panic!("expected stale pin after rollback, got {other:?}"),
    }
}
