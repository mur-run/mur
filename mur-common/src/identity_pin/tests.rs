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
fn rekey_repin_moves_the_trusted_head() {
    let (tmp, home, id) = agent();
    pin_from_disk(tmp.path(), AGENT).unwrap();
    let next = AgentIdentity::generate();
    std::fs::write(home.join(PUBKEY_FILE), next.pubkey_text()).unwrap();
    write_chain(&home, &[bootstrap(&id), rotation(&id, &next, 0)]);
    pin_from_disk(tmp.path(), AGENT).unwrap();
    let t = head(&home);
    assert_eq!((t.head_pubkey, t.head_key_version), (next.pubkey_text(), 1));
    assert_eq!(t.chain.map(|c| c.len()), Some(2));
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
