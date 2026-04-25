use mur_common::identity::{
    AgentIdentity, ChainError, ChainOptions, RotationAttestation, RotationReason, verify_chain,
};

const TEST_UUID: &str = "01JQX4TM8Y9K7VQH6B2N3R5DPE";

fn bootstrap(pubkey: &str) -> RotationAttestation {
    RotationAttestation::new(
        TEST_UUID,
        "",
        pubkey,
        0,
        0,
        "2026-04-25T10:00:00+08:00",
        RotationReason::Scheduled,
    )
    .into_bootstrap()
}

fn signed_step(
    old: &AgentIdentity,
    new: &AgentIdentity,
    old_v: u32,
    new_v: u32,
    reason: RotationReason,
) -> RotationAttestation {
    let mut a = RotationAttestation::new(
        TEST_UUID,
        old.pubkey_text(),
        new.pubkey_text(),
        old_v,
        new_v,
        format!("2026-04-25T11:0{old_v}:00+08:00"),
        reason,
    );
    a.sign(old.signing_key());
    a
}

#[test]
fn empty_chain_is_missing_bootstrap() {
    let err = verify_chain(&[], ChainOptions::default()).unwrap_err();
    assert!(matches!(err, ChainError::MissingBootstrap));
}

#[test]
fn chain_without_bootstrap_first_rejected() {
    let id = AgentIdentity::generate();
    let id2 = AgentIdentity::generate();
    let chain = vec![signed_step(&id, &id2, 0, 1, RotationReason::Scheduled)];
    let err = verify_chain(&chain, ChainOptions::default()).unwrap_err();
    assert!(matches!(err, ChainError::MissingBootstrap));
}

#[test]
fn single_bootstrap_chain_ok() {
    let id = AgentIdentity::generate();
    let chain = vec![bootstrap(&id.pubkey_text())];
    let out = verify_chain(&chain, ChainOptions::default()).expect("ok");
    assert_eq!(out.head_key_version, 0);
    assert_eq!(out.head_pubkey, id.pubkey_text());
    assert_eq!(out.length, 1);
}

#[test]
fn five_step_chain_ok() {
    let mut chain = vec![];
    let mut prev = AgentIdentity::generate();
    chain.push(bootstrap(&prev.pubkey_text()));
    for v in 1..=5 {
        let next = AgentIdentity::generate();
        chain.push(signed_step(
            &prev,
            &next,
            v - 1,
            v,
            RotationReason::Scheduled,
        ));
        prev = next;
    }
    let out = verify_chain(&chain, ChainOptions::default()).expect("ok");
    assert_eq!(out.head_key_version, 5);
    assert_eq!(out.head_pubkey, prev.pubkey_text());
    assert_eq!(out.length, 6);
}

#[test]
fn version_skip_rejected() {
    let mut chain = vec![];
    let prev = AgentIdentity::generate();
    let next = AgentIdentity::generate();
    let next2 = AgentIdentity::generate();
    chain.push(bootstrap(&prev.pubkey_text()));
    chain.push(signed_step(&prev, &next, 0, 1, RotationReason::Scheduled));
    // skip key_version=2 → jump to 3
    chain.push(signed_step(&next, &next2, 1, 3, RotationReason::Scheduled));

    let err = verify_chain(&chain, ChainOptions::default()).unwrap_err();
    assert!(matches!(
        err,
        ChainError::VersionSkip {
            expected: 2,
            got: 3
        }
    ));
}

#[test]
fn pubkey_discontinuity_rejected() {
    let prev = AgentIdentity::generate();
    let next = AgentIdentity::generate();
    let other = AgentIdentity::generate();
    let mut chain = vec![bootstrap(&prev.pubkey_text())];
    // Tamper: next step's old_pubkey is `other`, not `prev`
    let mut step = signed_step(&prev, &next, 0, 1, RotationReason::Scheduled);
    step.old_pubkey = other.pubkey_text();
    // re-sign with `other` so signature would technically validate against
    // `other`, but the chain check sees the discontinuity FIRST.
    step.sign(other.signing_key());
    chain.push(step);
    let err = verify_chain(&chain, ChainOptions::default()).unwrap_err();
    assert!(matches!(
        err,
        ChainError::PubkeyDiscontinuity { at_version: 1 }
    ));
}

#[test]
fn duplicate_version_rejected() {
    let prev = AgentIdentity::generate();
    let next = AgentIdentity::generate();
    let next2 = AgentIdentity::generate();
    let mut chain = vec![bootstrap(&prev.pubkey_text())];
    chain.push(signed_step(&prev, &next, 0, 1, RotationReason::Scheduled));
    // Second step claims to be ALSO key_version=1
    let mut dup = signed_step(&next, &next2, 1, 1, RotationReason::Scheduled);
    dup.new_key_version = 1; // belt and suspenders
    chain.push(dup);
    let err = verify_chain(&chain, ChainOptions::default()).unwrap_err();
    // Either duplicate or version skip is acceptable — both flag the issue.
    assert!(
        matches!(
            err,
            ChainError::DuplicateVersion(1) | ChainError::VersionSkip { .. }
        ),
        "expected DuplicateVersion or VersionSkip, got {err:?}"
    );
}

#[test]
fn bad_signature_rejected() {
    let prev = AgentIdentity::generate();
    let next = AgentIdentity::generate();
    let mut chain = vec![bootstrap(&prev.pubkey_text())];
    let mut step = signed_step(&prev, &next, 0, 1, RotationReason::Scheduled);
    // Tamper after signing
    step.new_pubkey = AgentIdentity::generate().pubkey_text();
    chain.push(step);
    let err = verify_chain(&chain, ChainOptions::default()).unwrap_err();
    assert!(matches!(
        err,
        ChainError::BadSignature { at_version: 1, .. }
    ));
}

#[test]
fn emergency_in_chain_rejected_by_default() {
    let prev = AgentIdentity::generate();
    let next = AgentIdentity::generate();
    let mut chain = vec![bootstrap(&prev.pubkey_text())];
    let emerg = RotationAttestation::new(
        TEST_UUID,
        prev.pubkey_text(),
        next.pubkey_text(),
        0,
        1,
        "2026-04-25T11:00:00+08:00",
        RotationReason::Emergency,
    );
    // No signature, no allow_emergency
    chain.push(emerg);
    let err = verify_chain(&chain, ChainOptions::default()).unwrap_err();
    assert!(matches!(
        err,
        ChainError::EmergencyDisallowed { at_version: 1 }
    ));
}

#[test]
fn emergency_in_chain_allowed_with_opt_in() {
    let prev = AgentIdentity::generate();
    let next = AgentIdentity::generate();
    let mut chain = vec![bootstrap(&prev.pubkey_text())];
    let emerg = RotationAttestation::new(
        TEST_UUID,
        prev.pubkey_text(),
        next.pubkey_text(),
        0,
        1,
        "2026-04-25T11:00:00+08:00",
        RotationReason::Emergency,
    );
    chain.push(emerg);
    let out = verify_chain(
        &chain,
        ChainOptions {
            allow_emergency: true,
        },
    )
    .expect("emergency-allowed must pass");
    assert_eq!(out.head_key_version, 1);
}

#[test]
fn hundred_step_chain() {
    let mut chain = vec![];
    let mut prev = AgentIdentity::generate();
    chain.push(bootstrap(&prev.pubkey_text()));
    for v in 1..=100u32 {
        let next = AgentIdentity::generate();
        chain.push(signed_step(
            &prev,
            &next,
            v - 1,
            v,
            RotationReason::Scheduled,
        ));
        prev = next;
    }
    let out = verify_chain(&chain, ChainOptions::default()).expect("100-step ok");
    assert_eq!(out.head_key_version, 100);
    assert_eq!(out.length, 101);
}
