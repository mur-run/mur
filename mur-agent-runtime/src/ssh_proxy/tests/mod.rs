//! Phase 2 tests (plan §"Phase 2 tests").

mod fixtures;
#[cfg(unix)]
mod listener;
mod rsa_vector;
mod session;
mod sign_binding;

use fixtures::*;

use super::keys::{BindError, fingerprint, verify_session_bind};
use super::policy::{PinTrust, Policy};
use super::refusal::{Refusal, RefusalCode};
use super::wire::{self, Answer, Identity, MAX_FRAME_BYTES, Request, WireError};
use mur_common::agent::{SshEntitlement, SshHostRule, SshKeyRule};

// ---- wire ----

#[test]
fn request_round_trips() {
    for req in [
        Request::RequestIdentities,
        Request::SignRequest {
            key_blob: vec![1, 2, 3],
            data: b"payload".to_vec(),
            flags: 4,
        },
        Request::SessionBind(wire::SessionBind {
            host_key: vec![9; 40],
            session_id: vec![7; 32],
            signature: vec![5; 64],
            is_forwarding: true,
        }),
    ] {
        let frame = req.encode();
        let n = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(n, frame.len() - 4);
        assert_eq!(wire::decode_request(&frame[4..]).unwrap(), req);
    }
}

#[test]
fn answer_round_trips() {
    for ans in [
        Answer::Identities(vec![
            Identity {
                key_blob: vec![1; 51],
                comment: b"a".to_vec(),
            },
            Identity {
                key_blob: vec![2; 51],
                comment: vec![],
            },
        ]),
        Answer::Identities(vec![]),
        Answer::SignResponse(vec![3; 83]),
        Answer::Success,
        Answer::Failure,
    ] {
        let frame = ans.encode();
        assert_eq!(wire::decode_answer(&frame[4..]).unwrap(), ans);
    }
}

/// Fuzz-style: every truncation of every valid message, and one byte of
/// garbage appended, must be an error — never a panic, never a success.
#[test]
fn every_truncation_and_extension_is_rejected() {
    let ed = Ed::new();
    let bodies = [
        sign_body(&ed.key_blob()),
        bind_body(&ed.key_blob(), b"sid", &ed.sign_blob(b"sid")),
    ];
    for body in &bodies {
        assert!(wire::decode_request(body).is_ok());
        for cut in 0..body.len() {
            assert!(wire::decode_request(&body[..cut]).is_err(), "cut at {cut}");
        }
        let mut long = body.clone();
        long.push(0);
        assert_eq!(wire::decode_request(&long), Err(WireError::TrailingBytes));
    }
}

#[test]
fn unknown_types_and_lengths_are_refused() {
    // 17 = SSH_AGENTC_ADD_IDENTITY: an agent may not be given keys through MUR.
    for t in [0u8, 1, 17, 19, 25, 255] {
        assert_eq!(wire::decode_request(&[t]), Err(WireError::UnknownType(t)));
    }
    assert!(wire::check_len(0).is_err());
    assert!(wire::check_len(MAX_FRAME_BYTES as u32 + 1).is_err());
    assert_eq!(wire::check_len(MAX_FRAME_BYTES as u32), Ok(MAX_FRAME_BYTES));
}

#[test]
fn absurd_identity_count_does_not_allocate() {
    let mut b = vec![wire::SSH_AGENT_IDENTITIES_ANSWER];
    b.extend_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(wire::decode_answer(&b), Err(WireError::Truncated));
}

// ---- fingerprints and session-bind ----

#[test]
fn fingerprint_matches_openssh_format() {
    // `ssh-keygen -lf` of an all-zero ed25519 key prints this.
    let blob = s(&[b"ssh-ed25519", &[0u8; 32]]);
    let fp = fingerprint(&blob);
    assert!(fp.starts_with("SHA256:") && fp.len() == 7 + 43, "{fp}");
    assert!(!fp.ends_with('='));
}

#[test]
fn ed25519_and_ecdsa_binds_verify_and_tampering_fails() {
    let hosts: Vec<Box<dyn TestHost>> = vec![
        Box::new(Ed::new()),
        Box::new(Ec::new(false)),
        Box::new(Ec::new(true)),
    ];
    for h in hosts {
        let sid = b"session identifier from the kex";
        let sig = h.sign_blob(sid);
        assert_eq!(verify_session_bind(&h.key_blob(), sid, &sig), Ok(()));
        assert_eq!(
            verify_session_bind(&h.key_blob(), b"another session", &sig),
            Err(BindError::BadSignature)
        );
        let mut bad = sig.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(verify_session_bind(&h.key_blob(), sid, &bad).is_err());
    }
}

#[test]
fn rsa_sha2_binds_verify_and_sha1_is_refused() {
    let key = rsa_key_blob();
    let sid = rsa_vector::SESSION_ID;
    assert_eq!(verify_session_bind(&key, sid, &rsa_sig_blob(false)), Ok(()));
    assert_eq!(verify_session_bind(&key, sid, &rsa_sig_blob(true)), Ok(()));
    assert_eq!(
        verify_session_bind(&key, b"other", &rsa_sig_blob(false)),
        Err(BindError::BadSignature)
    );
    let sha1 = s(&[b"ssh-rsa", &hex(rsa_vector::SIG_SHA256_HEX)]);
    assert_eq!(
        verify_session_bind(&key, sid, &sha1),
        Err(BindError::Unsupported("ssh-rsa".into()))
    );
}

#[test]
fn unsupported_and_mismatched_algorithms_fail_closed() {
    let p521 = s(&[b"ecdsa-sha2-nistp521", b"nistp521", &[4; 133]]);
    assert_eq!(
        verify_session_bind(&p521, b"x", &s(&[b"ecdsa-sha2-nistp521", b""])),
        Err(BindError::Unsupported("ecdsa-sha2-nistp521".into()))
    );
    let ed = Ed::new();
    let ec_sig = Ec::new(false).sign_blob(b"x");
    assert!(matches!(
        verify_session_bind(&ed.key_blob(), b"x", &ec_sig),
        Err(BindError::TypeMismatch { .. })
    ));
    assert_eq!(
        verify_session_bind(b"\0\0", b"x", &ed.sign_blob(b"x")),
        Err(BindError::Malformed)
    );
}

// ---- policy ----

pub(crate) fn policy_for(key_fp: &str, host_fp: &str) -> Policy {
    Policy::build(
        &SshEntitlement {
            keys: vec![SshKeyRule {
                fingerprint: key_fp.into(),
                label: Some("deploy".into()),
                allow_hosts: vec![SshHostRule {
                    host_key: host_fp.into(),
                    label: None,
                }],
            }],
        },
        PinTrust::Pinned,
        true,
    )
}

#[test]
fn policy_is_default_deny_per_key_and_host() {
    let (k, h, other) = (
        fingerprint(b"key"),
        fingerprint(b"host"),
        fingerprint(b"other"),
    );
    let p = policy_for(&k, &h);
    assert!(p.lists(&k) && !p.lists(&other));
    assert!(p.check_sign(&k, Some(&h)).is_ok());
    let code = |r: Result<_, Refusal>| r.unwrap_err().code;
    assert_eq!(
        code(p.check_sign(&other, Some(&h))),
        RefusalCode::KeyNotAllowed
    );
    assert_eq!(
        code(p.check_sign(&k, Some(&other))),
        RefusalCode::HostNotAllowed
    );
    assert_eq!(code(p.check_sign(&k, None)), RefusalCode::NoSessionBind);
}

#[test]
fn malformed_fingerprint_makes_the_whole_policy_refuse() {
    let p = policy_for("SHA256:short", &fingerprint(b"h"));
    assert!(matches!(p, Policy::RefuseAll(RefusalCode::PolicyInvalid)));
    assert_eq!(
        p.check_sign("SHA256:short", None).unwrap_err().code,
        RefusalCode::PolicyInvalid
    );
}

#[test]
fn unpinned_policy_refuses_only_where_the_platform_says_so() {
    let ent = SshEntitlement::default();
    assert!(matches!(
        Policy::build(&ent, PinTrust::Unpinned, true),
        Policy::RefuseAll(RefusalCode::PolicyUnpinned)
    ));
    assert!(matches!(
        Policy::build(&ent, PinTrust::Unpinned, false),
        Policy::Enforce(_)
    ));
    assert_eq!(Policy::UNPINNED_IS_UNTRUSTED, cfg!(target_os = "linux"));
}

// ---- refusal text ----

#[test]
fn no_session_bind_message_is_the_spec_text() {
    let m = Refusal::new(RefusalCode::NoSessionBind)
        .key("SHA256:abc123...", Some("github-deploy".into()))
        .message();
    assert_eq!(
        m,
        "ssh-agent proxy refused a signature for key github-deploy (SHA256:abc123...):\n\
         the client did not identify the destination host (no session-bind@openssh.com).\n\
         This is not a key or policy problem. MUR needs the destination host to enforce\n\
         the policy, and this ssh client does not send it. Use OpenSSH 8.9 or newer."
    );
}

#[test]
fn every_code_is_distinct_and_named_in_its_message() {
    let mut seen = std::collections::HashSet::new();
    for c in RefusalCode::ALL {
        assert!(seen.insert(c.as_str()));
        if c != RefusalCode::NoSessionBind {
            assert!(Refusal::new(c).message().contains(c.as_str()));
        }
    }
}
