//! Spec "Binding sign requests to the verified session-bind" (issue #2213).
//! A signature is only for the session whose bind the proxy verified. Every
//! refusal here must leave the fake upstream without a single sign request.

use super::fixtures::*;
use super::gate::FakeGate;
use super::session::{FakeUpstream, World};
use crate::ssh_proxy::gate::GateOutcome;
use crate::ssh_proxy::session::Session;
use crate::ssh_proxy::wire::{self, Answer};
use mur_common::ssh_proxy_refusals;

/// What one connection produced: the last answer, the refusal code and
/// message recorded for it, and how many sign requests reached upstream.
struct Outcome {
    answer: Answer,
    code: Option<String>,
    message: String,
    forwarded_signs: usize,
}

/// Send `binds` in order, then one sign request for `key` with `data`. The
/// gate denies, so a request that passes every binding rule and the policy
/// ends at `hitl_denied` and is still never forwarded.
async fn run(w: &World, binds: &[Vec<u8>], key: usize, data: &[u8]) -> Outcome {
    let (p, up, sink) = (w.policy(), w.upstream(), w.sink());
    let gate = FakeGate::new(GateOutcome::Deny);
    let mut sess = Session::new(&p, &up, &sink, &*gate);
    for b in binds {
        assert_eq!(sess.handle(b).await, Answer::Success, "setup bind refused");
    }
    let answer = sess
        .handle(&sign_data_body(&w.keys[key].key_blob(), data))
        .await;
    outcome(w, &up, answer)
}

fn outcome(w: &World, up: &FakeUpstream, answer: Answer) -> Outcome {
    let last = ssh_proxy_refusals::read(w.home_path()).into_iter().next();
    Outcome {
        answer,
        code: last.as_ref().map(|r| r.code.clone()),
        message: last.map(|r| r.message).unwrap_or_default(),
        forwarded_signs: up
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f[4] == wire::SSH_AGENTC_SIGN_REQUEST)
            .count(),
    }
}

fn bind_for(host: &Ed, sid: &[u8]) -> Vec<u8> {
    bind_body(&host.key_blob(), sid, &host.sign_blob(sid))
}

fn assert_refused(o: &Outcome, want: &str) {
    assert_eq!(o.answer, Answer::Failure);
    assert_eq!(o.code.as_deref(), Some(want), "message: {}", o.message);
    assert_eq!(
        o.forwarded_signs, 0,
        "{want}: a sign request reached upstream"
    );
}

/// Test 1. The matching request passes every binding rule and the policy,
/// and reaches the approval gate (which denies here).
#[tokio::test]
async fn matching_session_id_reaches_the_policy() {
    let w = World::new();
    let key = w.keys[0].key_blob();
    let o = run(&w, &[w.good_bind()], 0, &Userauth::new(SID, &key).encode()).await;
    assert_refused(&o, "hitl_denied");
}

/// Test 2. The attack: bind proves session H1, signature is for H2.
#[tokio::test]
async fn different_session_id_is_session_id_mismatch() {
    let w = World::new();
    let key = w.keys[0].key_blob();
    let o = run(
        &w,
        &[w.good_bind()],
        0,
        &Userauth::new(b"H2", &key).encode(),
    )
    .await;
    assert_refused(&o, "session_id_mismatch");
}

/// Test 3. Only the most recent bind counts.
#[tokio::test]
async fn an_earlier_bind_is_not_current() {
    let w = World::new();
    let key = w.keys[0].key_blob();
    let binds = [bind_for(&w.host, b"H1"), bind_for(&w.host, b"H2")];
    let o = run(&w, &binds, 0, &Userauth::new(b"H1", &key).encode()).await;
    assert_refused(&o, "session_id_mismatch");
}

/// Test 4. Hostbound: the server host key inside the data must be the bind's.
#[tokio::test]
async fn hostbound_host_key_must_match_the_bind() {
    let w = World::new();
    let key = w.keys[0].key_blob();
    let other = Ed::new().key_blob();
    let bad = Userauth::new(SID, &key).hostbound(&other).encode();
    assert_refused(
        &run(&w, &[w.good_bind()], 0, &bad).await,
        "sign_request_inconsistent",
    );
    let host = w.host.key_blob();
    let good = Userauth::new(SID, &key).hostbound(&host).encode();
    assert_refused(&run(&w, &[w.good_bind()], 0, &good).await, "hitl_denied");
}

/// Test 5. The key inside the data must be the key asked to sign.
#[tokio::test]
async fn userauth_key_must_be_the_requested_key() {
    let w = World::new();
    let other = w.keys[1].key_blob();
    let data = Userauth::new(SID, &other).encode();
    assert_refused(
        &run(&w, &[w.good_bind()], 0, &data).await,
        "sign_request_inconsistent",
    );
}

/// Test 6. Anything that is not exactly a userauth request.
#[tokio::test]
async fn non_userauth_data_is_refused() {
    let w = World::new();
    let key = w.keys[0].key_blob();
    let valid = Userauth::new(SID, &key).encode();
    let mut wrong_service = Userauth::new(SID, &key);
    wrong_service.service = b"ssh-userauth";
    let mut wrong_method = Userauth::new(SID, &key);
    wrong_method.method = b"password";
    // `ssh-keygen -Y sign` / git commit signing (PROTOCOL.sshsig).
    let sshsig = [
        b"SSHSIG".as_slice(),
        &s(&[b"git", b"", b"sha512", &[7; 64]]),
    ]
    .concat();
    let cases: [(&str, Vec<u8>); 6] = [
        ("wrong service", wrong_service.encode()),
        ("wrong method", wrong_method.encode()),
        ("trailing byte", [valid.clone(), vec![0]].concat()),
        ("truncated", valid[..valid.len() - 1].to_vec()),
        ("empty", Vec::new()),
        ("sshsig", sshsig),
    ];
    for (name, data) in cases {
        let o = run(&w, &[w.good_bind()], 0, &data).await;
        assert_eq!(o.code.as_deref(), Some("sign_data_not_userauth"), "{name}");
        assert_eq!(o.forwarded_signs, 0, "{name}");
    }
    let o = run(
        &w,
        &[w.good_bind()],
        0,
        &[b"SSHSIG".as_slice(), &[0; 8]].concat(),
    )
    .await;
    assert!(
        o.message.contains("not supported"),
        "SSHSIG refusal must say signing is unsupported, got: {}",
        o.message
    );
}

/// Test 7. `ssh -A` through the proxy is refused in v1.
#[tokio::test]
async fn forwarding_bind_is_forwarding_not_supported() {
    let w = World::new();
    let key = w.keys[0].key_blob();
    let fwd = bind_body_fwd(&w.host.key_blob(), SID, &w.host.sign_blob(SID), true);
    let o = run(&w, &[fwd], 0, &Userauth::new(SID, &key).encode()).await;
    assert_refused(&o, "forwarding_not_supported");
}

/// Test 8. Order: binding checks run before the key/host lookup (so no HITL
/// prompt for a mismatch), but after `no_session_bind`.
#[tokio::test]
async fn binding_checks_run_before_policy_and_after_bind_presence() {
    let w = World::new();
    // Key 1 is not in the policy: the mismatch must still win.
    let key1 = w.keys[1].key_blob();
    let o = run(
        &w,
        &[w.good_bind()],
        1,
        &Userauth::new(b"H2", &key1).encode(),
    )
    .await;
    assert_refused(&o, "session_id_mismatch");
    // No bind at all: that is still the reported reason, even for junk data.
    let o = run(&w, &[], 0, b"junk").await;
    assert_refused(&o, "no_session_bind");
}

/// Rule 7: refusal messages never carry the session id or user name in full.
#[tokio::test]
async fn mismatch_message_hides_session_id_and_user() {
    use base64::Engine as _;
    let w = World::new();
    let key = w.keys[0].key_blob();
    let (bound, asked) = ([0xab; 32], [0xcd; 32]);
    let mut data = Userauth::new(&asked, &key);
    data.user = b"alice-private-login";
    let o = run(&w, &[bind_for(&w.host, &bound)], 0, &data.encode()).await;
    assert_refused(&o, "session_id_mismatch");
    assert!(o.message.contains("different SSH session"), "{}", o.message);
    for sid in [bound, asked] {
        let hex: String = sid.iter().map(|b| format!("{b:02x}")).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(sid);
        assert!(!o.message.to_lowercase().contains(&hex), "{}", o.message);
        assert!(
            !o.message.contains(b64.trim_end_matches('=')),
            "{}",
            o.message
        );
    }
    assert!(!o.message.contains("alice-private-login"), "{}", o.message);
}
