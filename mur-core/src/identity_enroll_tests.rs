//! A1: one test per row of the `enroll` table, plus the crash and re-run
//! cases. Every partial state must fail closed and leave the disk untouched.

use super::*;
use mur_common::identity::human::HUMAN_IDENTITY_DIR;
use tempfile::TempDir;

fn snapshot(mur: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for dir in [human_identity_dir(mur), human_key_dir(mur)] {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd {
            let p = e.unwrap().path();
            out.push((p.clone(), fs::read(&p).unwrap()));
        }
    }
    out.sort();
    out
}

fn assert_refused_untouched(mur: &Path, needle: &str) {
    let before = snapshot(mur);
    let err = enroll(mur).unwrap_err().to_string();
    assert!(err.contains(needle), "{err}");
    assert_eq!(snapshot(mur), before, "a refused enroll wrote something");
}

#[test]
fn absent_absent_creates_both_and_the_bootstrap() {
    let tmp = TempDir::new().unwrap();
    let mur = tmp.path();
    let Enrollment::Created { pubkey } = enroll(mur).unwrap() else {
        panic!("expected Created");
    };
    assert_eq!(load_pubkey(mur).unwrap(), pubkey);
    let chain = rotation_chain(mur).unwrap();
    assert_eq!(chain.head_key_version(), 0);
    assert!(chain.entries()[0].bootstrap);
    assert_eq!(HumanSigner::load(mur).unwrap().pubkey(), pubkey);
}

#[test]
fn enroll_twice_returns_existing_same_pubkey() {
    let tmp = TempDir::new().unwrap();
    let first = enroll(tmp.path()).unwrap();
    let before = snapshot(tmp.path());
    let second = enroll(tmp.path()).unwrap();
    assert_eq!(
        second,
        Enrollment::Existing {
            pubkey: first.pubkey()
        }
    );
    assert_eq!(snapshot(tmp.path()), before);
    assert_eq!(ensure_enrolled(tmp.path()).unwrap(), first.pubkey());
}

#[test]
fn pub_present_key_absent_is_refused_and_names_the_key() {
    let tmp = TempDir::new().unwrap();
    enroll(tmp.path()).unwrap();
    fs::remove_file(human_private_key_path(tmp.path())).unwrap();
    assert_refused_untouched(
        tmp.path(),
        &human_private_key_path(tmp.path()).display().to_string(),
    );
}

#[cfg(unix)]
#[test]
fn pub_present_key_unreadable_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let tmp = TempDir::new().unwrap();
    enroll(tmp.path()).unwrap();
    let keys = human_key_dir(tmp.path());
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o000)).unwrap();
    let got = enroll(tmp.path());
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700)).unwrap();
    let err = got.unwrap_err().to_string();
    assert!(err.contains("refusing to enroll"), "{err}");
}

/// TR:112-113 — `keys/.human/` exists, `identity/` does not: never mint.
#[test]
fn key_present_pub_absent_is_refused() {
    let tmp = TempDir::new().unwrap();
    enroll(tmp.path()).unwrap();
    fs::remove_dir_all(human_identity_dir(tmp.path())).unwrap();
    assert_refused_untouched(tmp.path(), "public half");
}

#[test]
fn pub_and_key_mismatch_is_refused() {
    let tmp = TempDir::new().unwrap();
    enroll(tmp.path()).unwrap();
    fs::write(
        human_pubkey_path(tmp.path()),
        AgentIdentity::generate().pubkey_text(),
    )
    .unwrap();
    assert_refused_untouched(tmp.path(), "does not match");
}

/// Crash between the key and the attestation: detected, never re-minted.
#[test]
fn enroll_crash_after_key_before_attestation_is_detected_not_reminted() {
    let tmp = TempDir::new().unwrap();
    let mur = tmp.path();
    AgentIdentity::generate()
        .save_split(&human_identity_dir(mur), &human_key_dir(mur))
        .unwrap();
    assert_refused_untouched(mur, "rotation chain");
    assert!(!human_rotations_path(mur).exists());
}

#[test]
fn a_tampered_chain_is_refused() {
    let tmp = TempDir::new().unwrap();
    enroll(tmp.path()).unwrap();
    fs::write(human_rotations_path(tmp.path()), "garbage\n").unwrap();
    assert_refused_untouched(tmp.path(), "rotation chain");
}

#[test]
fn write_bootstrap_never_replaces_a_chain() {
    let tmp = TempDir::new().unwrap();
    enroll(tmp.path()).unwrap();
    let before = fs::read(human_rotations_path(tmp.path())).unwrap();
    assert!(write_bootstrap(tmp.path(), &AgentIdentity::generate()).is_err());
    assert_eq!(fs::read(human_rotations_path(tmp.path())).unwrap(), before);
}

/// D-8: nothing secret under the agent-readable `identity/`.
#[test]
fn save_and_rekey_leave_no_key_material_under_identity() {
    let tmp = TempDir::new().unwrap();
    enroll(tmp.path()).unwrap();
    for e in fs::read_dir(human_identity_dir(tmp.path())).unwrap() {
        let name = e.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            !name.contains(".key"),
            "key material in {HUMAN_IDENTITY_DIR}/: {name}"
        );
    }
    // The rekey half of this lands with A2 (`mur identity rekey`), which
    // must move `.prev` under keys/.human explicitly.
}

#[cfg(unix)]
#[test]
fn enrolled_dirs_and_key_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    enroll(tmp.path()).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&human_identity_dir(tmp.path())), 0o700);
    assert_eq!(mode(&human_key_dir(tmp.path())), 0o700);
    assert_eq!(mode(&human_private_key_path(tmp.path())), 0o600);
}

#[test]
fn signer_signs_under_the_enrolled_key() {
    let tmp = TempDir::new().unwrap();
    let pubkey = ensure_enrolled(tmp.path()).unwrap();
    let signer = HumanSigner::load(tmp.path()).unwrap();
    let sig = signer.sign_bytes(b"hello");
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&pubkey).unwrap();
    vk.verify_strict(b"hello", &ed25519_dalek::Signature::from_bytes(&sig))
        .unwrap();
}
