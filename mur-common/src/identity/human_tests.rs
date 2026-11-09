//! A1 read side: `load_pubkey`, `rotation_chain`, and the `identity/` →
//! `keys/.human` mapping in `private_key_dir`.

use super::*;
use crate::identity::{AgentIdentity, RotationAttestation, RotationReason, private_key_dir};

fn write_chain(mur: &Path, entries: &[RotationAttestation]) {
    let dir = human_identity_dir(mur);
    fs::create_dir_all(&dir).unwrap();
    let body: String = entries
        .iter()
        .map(|a| format!("{}\n", serde_json::to_string(a).unwrap()))
        .collect();
    fs::write(human_rotations_path(mur), body).unwrap();
}

fn bootstrap(id: &AgentIdentity) -> RotationAttestation {
    RotationAttestation::new(
        "u",
        "",
        id.pubkey_text(),
        0,
        0,
        "2026-11-09T00:00:00Z",
        RotationReason::Scheduled,
    )
    .into_bootstrap()
}

fn enrolled(mur: &Path) -> AgentIdentity {
    let id = AgentIdentity::generate();
    id.save_split(&human_identity_dir(mur), &human_key_dir(mur))
        .unwrap();
    write_chain(mur, &[bootstrap(&id)]);
    id
}

#[test]
fn identity_dir_maps_its_private_half_to_keys_dot_human() {
    let mur = Path::new("/m");
    assert_eq!(
        private_key_dir(&human_identity_dir(mur)),
        human_key_dir(mur)
    );
    // An agent that happens to be called `identity` is still an agent.
    assert_eq!(
        private_key_dir(&mur.join("agents").join("identity")),
        mur.join("keys").join("identity")
    );
}

/// `save(<home>/identity)` must not write the private key into the
/// agent-readable public directory (rev 4, D-8).
#[test]
fn save_into_identity_dir_puts_no_key_material_there() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();
    AgentIdentity::generate()
        .save(&human_identity_dir(mur))
        .unwrap();
    assert!(human_pubkey_path(mur).exists());
    assert!(human_private_key_path(mur).exists());
    for e in fs::read_dir(human_identity_dir(mur)).unwrap() {
        let name = e.unwrap().file_name();
        assert!(
            !name.to_string_lossy().contains(".key"),
            "key material under identity/: {name:?}"
        );
    }
}

/// Existing `save` callers see byte-identical layout.
#[test]
fn save_split_existing_callers_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let agent = tmp.path().join("agents").join("a");
    let id = AgentIdentity::generate();
    id.save(&agent).unwrap();
    let key = tmp.path().join("keys").join("a").join("identity.key");
    assert_eq!(fs::read(&key).unwrap(), id.secret_bytes_for_handoff());
    assert_eq!(
        fs::read_to_string(agent.join("identity.pub")).unwrap(),
        id.pubkey_text()
    );
    assert!(!agent.join("identity.key").exists());
    // Non-agent directories still keep both halves together.
    let host = tmp.path().join("commander");
    AgentIdentity::generate().save(&host).unwrap();
    assert!(host.join("identity.key").exists() && host.join("identity.pub").exists());
}

#[test]
fn save_split_refuses_to_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let (p, k) = (tmp.path().join("p"), tmp.path().join("k"));
    AgentIdentity::generate().save_split(&p, &k).unwrap();
    assert!(matches!(
        AgentIdentity::generate().save_split(&p, &k),
        Err(crate::identity::IdentityError::Exists(_))
    ));
}

#[test]
fn load_pubkey_absent_is_not_enrolled() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        load_pubkey(tmp.path()),
        Err(HumanIdentityError::NotEnrolled { .. })
    ));
}

#[test]
fn load_pubkey_reads_the_public_half() {
    let tmp = tempfile::tempdir().unwrap();
    let id = enrolled(tmp.path());
    assert_eq!(load_pubkey(tmp.path()).unwrap(), id.verifying_key_bytes());
}

/// A verifier inside the sandbox cannot read `keys/`; `load_pubkey` must not
/// need to.
#[cfg(unix)]
#[test]
fn load_pubkey_never_touches_keys_dir() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // root ignores mode bits
    }
    let tmp = tempfile::tempdir().unwrap();
    let id = enrolled(tmp.path());
    let keys = human_key_dir(tmp.path());
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o000)).unwrap();
    let got = load_pubkey(tmp.path());
    let chain = rotation_chain(tmp.path());
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(got.unwrap(), id.verifying_key_bytes());
    assert_eq!(chain.unwrap().head_pubkey(), id.verifying_key_bytes());
}

/// #1010: a denied stat is not "absent" — enrolling on it would mint a
/// second key.
#[cfg(unix)]
#[test]
fn denied_stat_is_unreadable_not_notfound() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    enrolled(tmp.path());
    let dir = human_identity_dir(tmp.path());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
    let got = load_pubkey(tmp.path());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(got, Err(HumanIdentityError::Unreadable { .. })),
        "{got:?}"
    );
}

#[test]
fn rotation_chain_verifies_and_reports_the_head() {
    let tmp = tempfile::tempdir().unwrap();
    let id = enrolled(tmp.path());
    let chain = rotation_chain(tmp.path()).unwrap();
    assert_eq!(chain.head_key_version(), 0);
    assert_eq!(chain.head_pubkey(), id.verifying_key_bytes());
    assert_eq!(chain.entries().len(), 1);
}

#[test]
fn rotation_chain_absent_is_not_enrolled() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        rotation_chain(tmp.path()),
        Err(HumanIdentityError::NotEnrolled { .. })
    ));
}

/// A garbage line is not skipped — skipping is how a planted line slips in.
#[test]
fn rotation_chain_with_a_bad_line_is_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    enrolled(tmp.path());
    let path = human_rotations_path(tmp.path());
    let mut body = fs::read_to_string(&path).unwrap();
    body.push_str("not json\n");
    fs::write(&path, body).unwrap();
    assert!(matches!(
        rotation_chain(tmp.path()),
        Err(HumanIdentityError::Invalid { .. })
    ));
}

#[test]
fn rotation_chain_whose_head_is_not_identity_pub_is_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    enrolled(tmp.path());
    write_chain(tmp.path(), &[bootstrap(&AgentIdentity::generate())]);
    assert!(matches!(
        rotation_chain(tmp.path()),
        Err(HumanIdentityError::Invalid { .. })
    ));
}

#[test]
fn rotation_chain_follows_a_signed_rotation() {
    let tmp = tempfile::tempdir().unwrap();
    let old = enrolled(tmp.path());
    let new = AgentIdentity::generate();
    let mut step = RotationAttestation::new(
        "u",
        old.pubkey_text(),
        new.pubkey_text(),
        0,
        1,
        "2026-11-09T01:00:00Z",
        RotationReason::Scheduled,
    );
    step.sign(old.signing_key());
    write_chain(tmp.path(), &[bootstrap(&old), step]);
    fs::write(human_pubkey_path(tmp.path()), new.pubkey_text()).unwrap();
    let chain = rotation_chain(tmp.path()).unwrap();
    assert_eq!(chain.head_key_version(), 1);
    assert_eq!(chain.head_pubkey(), new.verifying_key_bytes());
}
