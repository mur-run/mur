use super::*;
use mur_common::identity::human::{human_identity_dir, human_key_dir, rotation_chain};
use tempfile::TempDir;

use crate::identity_enroll::enroll;

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

use std::fs;
use std::path::Path;

#[test]
fn init_is_idempotent() {
    let t = TempDir::new().unwrap();
    init::run(t.path()).unwrap();
    let pk = mur_common::identity::human::load_pubkey(t.path()).unwrap();
    init::run(t.path()).unwrap();
    assert_eq!(
        mur_common::identity::human::load_pubkey(t.path()).unwrap(),
        pk
    );
    assert_eq!(rotation_chain(t.path()).unwrap().entries().len(), 1);
}

#[test]
fn show_json_fields_and_refuses_without_identity() {
    let t = TempDir::new().unwrap();
    assert!(show::run(t.path(), true).is_err());
    enroll(t.path()).unwrap();
    show::run(t.path(), true).unwrap();
    show::run(t.path(), false).unwrap();
}

#[test]
fn rekey_prev_lands_under_keys_never_identity() {
    let t = TempDir::new().unwrap();
    enroll(t.path()).unwrap();
    run(
        t.path(),
        IdentityAction::Rekey {
            reason: "scheduled".into(),
            yes: true,
        },
    );
    assert_eq!(
        files(&human_key_dir(t.path())),
        vec!["identity.key", "identity.key.prev"]
    );
    assert_eq!(
        files(&human_identity_dir(t.path())),
        vec!["identity.pub", "rotations.jsonl"]
    );
}

fn run(home: &Path, a: IdentityAction) {
    super::run(home, a).unwrap();
}

#[test]
fn rekey_appends_signed_by_old_key_and_bumps_head() {
    let t = TempDir::new().unwrap();
    let old = enroll(t.path()).unwrap().pubkey();
    run(
        t.path(),
        IdentityAction::Rekey {
            reason: "owner-change".into(),
            yes: true,
        },
    );
    let chain = rotation_chain(t.path()).unwrap();
    assert_eq!(chain.head_key_version(), 1);
    assert_eq!(chain.entries().len(), 2);
    assert_ne!(chain.head_pubkey(), old);
    let last = &chain.entries()[1];
    assert!(!last.signature.is_empty() && !last.bootstrap);
    // The new key is the one on disk and signs.
    let s = crate::identity_enroll::HumanSigner::load(t.path()).unwrap();
    assert_eq!(s.pubkey(), chain.head_pubkey());
}

#[test]
fn rekey_refuses_when_identity_missing() {
    let t = TempDir::new().unwrap();
    let err = rekey::run(t.path(), "scheduled", true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("mur identity init"), "{err}");
    assert!(!human_identity_dir(t.path()).exists());
}

#[test]
fn rekey_has_no_emergency() {
    let t = TempDir::new().unwrap();
    enroll(t.path()).unwrap();
    assert!(rekey::run(t.path(), "emergency", true).is_err());
    assert_eq!(rotation_chain(t.path()).unwrap().head_key_version(), 0);
}
