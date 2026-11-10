use super::*;
use crate::identity::{AgentIdentity, RotationReason, decode_pubkey};

const AGENT: &str = "qa";

fn bootstrap(id: &AgentIdentity) -> RotationAttestation {
    RotationAttestation::new(
        "u",
        "",
        id.pubkey_text(),
        0,
        0,
        "t0",
        RotationReason::Scheduled,
    )
    .into_bootstrap()
}

fn rotation(old: &AgentIdentity, new: &AgentIdentity, from: u32) -> RotationAttestation {
    let mut a = RotationAttestation::new(
        "u",
        old.pubkey_text(),
        new.pubkey_text(),
        from,
        from + 1,
        "t1",
        RotationReason::Scheduled,
    );
    a.sign(old.signing_key());
    a
}

fn write_chain(home: &Path, chain: &[RotationAttestation]) {
    let body: String = chain
        .iter()
        .map(|a| serde_json::to_string(a).unwrap() + "\n")
        .collect();
    std::fs::write(home.join(CHAIN_FILE), body).unwrap();
}

/// An installed agent `qa` with a real identity and bootstrap chain.
fn agent() -> (tempfile::TempDir, PathBuf, AgentIdentity) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agents").join(AGENT);
    std::fs::create_dir_all(&home).unwrap();
    let id = AgentIdentity::generate();
    id.save(&home).unwrap();
    write_chain(&home, &[bootstrap(&id)]);
    (tmp, home, id)
}

fn head(home: &Path) -> TrustedIdentity {
    resolve(home).expect("pinned").expect("usable")
}

#[test]
fn unpinned_agent_resolves_to_none() {
    let (_tmp, home, _) = agent();
    assert!(resolve(&home).is_none(), "no pin yet: legacy path");
}

#[test]
fn directory_outside_agents_is_never_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(split_agent_home(tmp.path()).is_none());
    assert!(resolve(tmp.path()).is_none());
}

#[test]
fn pin_from_disk_records_the_current_identity() {
    let (tmp, home, id) = agent();
    let pin = pin_from_disk(tmp.path(), AGENT).unwrap();
    assert_eq!(pin.pubkey, id.pubkey_text());
    assert_eq!(pin.key_version, 0);
    let t = head(&home);
    assert_eq!(t.head_pubkey, id.pubkey_text());
    assert_eq!(t.chain.as_deref(), Some(&[bootstrap(&id)][..]));
}

/// The #2318 attack: the agent writes a fresh keypair's public half and a
/// bootstrap chain for it. That chain verifies on its own; the pin ignores it.
#[test]
fn forged_chain_and_identity_pub_are_not_trusted() {
    let (tmp, home, id) = agent();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    let forged = AgentIdentity::generate();
    std::fs::write(home.join(PUBKEY_FILE), forged.pubkey_text()).unwrap();
    write_chain(&home, &[bootstrap(&forged)]);
    assert!(
        verify_chain(&[bootstrap(&forged)], strict()).is_ok(),
        "precondition: the forged chain verifies without an anchor"
    );
    assert_eq!(head(&home).head_pubkey, id.pubkey_text());
}

/// Appending a link from the pinned key the agent cannot sign (no private
/// half) changes nothing either: the pin, not the file, is read.
#[test]
fn appended_link_on_disk_is_not_trusted() {
    let (tmp, home, id) = agent();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    let attacker = AgentIdentity::generate();
    let mut link = rotation(&attacker, &AgentIdentity::generate(), 0);
    link.old_pubkey = id.pubkey_text();
    write_chain(&home, &[bootstrap(&id), link]);
    let t = head(&home);
    assert_eq!((t.head_pubkey, t.head_key_version), (id.pubkey_text(), 0));
}

#[test]
fn rekey_extends_the_pinned_head() {
    let (tmp, home, id) = agent();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    let next = AgentIdentity::generate();
    extend_pin(tmp.path(), AGENT, &rotation(&id, &next, 0)).unwrap();
    let t = head(&home);
    assert_eq!((t.head_pubkey, t.head_key_version), (next.pubkey_text(), 1));
    assert_eq!(t.chain.map(|c| c.len()), Some(2));
}

/// Review of #2340: the agent at version 1 rewrites its history to
/// `bootstrap(A) -> A-signed rotation to the real v1 key`, which verifies
/// without the real private key. A legitimate rekey then appends v2. The pin
/// must keep the genuine history, so version 0 never resolves to A.
#[test]
fn forged_history_is_not_imported_by_a_legitimate_rekey() {
    let (tmp, home, v0) = agent();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    let v1 = AgentIdentity::generate();
    extend_pin(tmp.path(), AGENT, &rotation(&v0, &v1, 0)).unwrap();

    let attacker = AgentIdentity::generate();
    let forged = [bootstrap(&attacker), rotation(&attacker, &v1, 0)];
    assert!(verify_chain(&forged, strict()).is_ok(), "precondition");
    write_chain(&home, &forged);

    let v2 = AgentIdentity::generate();
    let genuine = rotation(&v1, &v2, 1);
    let mut on_disk = forged.to_vec();
    on_disk.push(genuine.clone());
    write_chain(&home, &on_disk);
    extend_pin(tmp.path(), AGENT, &genuine).unwrap();

    let chain = head(&home).chain.expect("strict chain");
    assert_eq!(
        chain[0].new_pubkey,
        v0.pubkey_text(),
        "v0 is still the real key"
    );
    assert!(chain.iter().all(|a| a.new_pubkey != attacker.pubkey_text()));
    assert_eq!(chain.len(), 3);
}

#[test]
fn extension_must_continue_the_pinned_head() {
    let (tmp, _home, id) = agent();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    let other = AgentIdentity::generate();
    let next = AgentIdentity::generate();
    // Old key is not the pinned key.
    let wrong_key = rotation(&other, &next, 0);
    // Right key, wrong version.
    let wrong_version = rotation(&id, &next, 3);
    // Claims the pinned key, signed by another.
    let mut forged_sig = rotation(&other, &next, 0);
    forged_sig.old_pubkey = id.pubkey_text();
    for bad in [wrong_key, wrong_version, forged_sig, bootstrap(&next)] {
        assert!(matches!(
            extend_pin(tmp.path(), AGENT, &bad),
            Err(IdentityPinError::NotAnExtension { .. })
        ));
    }
    assert_eq!(
        load_pin(tmp.path(), AGENT).unwrap().unwrap().pubkey,
        id.pubkey_text()
    );
}

#[test]
fn rekey_of_an_unpinned_agent_pins_only_the_new_head() {
    let (tmp, home, id) = agent();
    let next = AgentIdentity::generate();
    extend_pin(tmp.path(), AGENT, &rotation(&id, &next, 0)).unwrap();
    let t = head(&home);
    assert_eq!((t.head_pubkey, t.head_key_version), (next.pubkey_text(), 1));
    assert!(t.chain.is_none(), "no on-disk history imported");
}

fn emergency(new: &AgentIdentity, from: u32) -> RotationAttestation {
    RotationAttestation::new(
        "u",
        "",
        new.pubkey_text(),
        from,
        from + 1,
        "t2",
        RotationReason::Emergency,
    )
}

#[test]
fn fail_closed_pin_trusts_no_key_and_ignores_disk() {
    let (tmp, home, id) = agent();
    write_fail_closed(tmp.path(), AGENT).unwrap();
    assert_eq!(resolve(&home), Some(None), "pinned, but nothing trusted");
    // A well-formed forged identity on disk still changes nothing.
    let forged = AgentIdentity::generate();
    std::fs::write(home.join(PUBKEY_FILE), forged.pubkey_text()).unwrap();
    write_chain(&home, &[bootstrap(&forged)]);
    assert_eq!(resolve(&home), Some(None));
    // Only an emergency rotation resets it.
    let next = AgentIdentity::generate();
    assert!(extend_pin(tmp.path(), AGENT, &rotation(&id, &next, 0)).is_err());
    extend_pin(tmp.path(), AGENT, &emergency(&next, 0)).unwrap();
    assert_eq!(head(&home).head_pubkey, next.pubkey_text());
}

#[test]
fn chain_head_disagreeing_with_identity_pub_is_refused() {
    let (tmp, home, _) = agent();
    std::fs::write(
        home.join(PUBKEY_FILE),
        AgentIdentity::generate().pubkey_text(),
    )
    .unwrap();
    assert!(matches!(
        pin_from_disk(tmp.path(), AGENT),
        Err(IdentityPinError::Inconsistent { .. })
    ));
}

#[test]
fn identity_without_chain_pins_head_only() {
    let (tmp, home, id) = agent();
    std::fs::remove_file(home.join(CHAIN_FILE)).unwrap();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    let t = head(&home);
    assert_eq!((t.head_pubkey, t.head_key_version), (id.pubkey_text(), 0));
    assert!(t.chain.is_none());
}

#[test]
fn corrupt_pin_fails_closed() {
    let (tmp, home, _) = agent();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    std::fs::write(pin_path(tmp.path(), AGENT), b"{not json").unwrap();
    assert_eq!(resolve(&home), Some(None), "unusable pin trusts no key");
}

#[test]
fn remove_and_rename_follow_the_agent() {
    let (tmp, _home, _) = agent();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    rename_pin(tmp.path(), AGENT, "qa2").unwrap();
    assert!(load_pin(tmp.path(), AGENT).unwrap().is_none());
    assert!(load_pin(tmp.path(), "qa2").unwrap().is_some());
    remove_pin(tmp.path(), "qa2").unwrap();
    remove_pin(tmp.path(), "qa2").unwrap();
    assert!(load_pin(tmp.path(), "qa2").unwrap().is_none());
}

#[test]
fn hostile_names_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    for bad in ["../x", "a/b", ""] {
        assert!(matches!(
            pin_from_disk(tmp.path(), bad),
            Err(IdentityPinError::Name(_))
        ));
    }
}

#[test]
fn load_pubkey_answers_with_the_pin() {
    let (tmp, home, id) = agent();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    std::fs::write(
        home.join(PUBKEY_FILE),
        AgentIdentity::generate().pubkey_text(),
    )
    .unwrap();
    assert_eq!(
        AgentIdentity::load_pubkey(&home).unwrap(),
        decode_pubkey(&id.pubkey_text()).unwrap()
    );
}
