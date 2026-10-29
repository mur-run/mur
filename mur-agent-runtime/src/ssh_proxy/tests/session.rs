//! Session tests against a fake upstream agent holding three keys. Each
//! refusal code a connection can produce is reached here, with the sink
//! (refusals file) asserted.

use std::sync::Mutex;

use super::fixtures::*;
use super::policy_for;
use crate::ssh_proxy::keys::fingerprint;
use crate::ssh_proxy::policy::{PinTrust, Policy};
use crate::ssh_proxy::refusal::{RefusalCode, RefusalSink};
use crate::ssh_proxy::session::{MAX_SESSION_BINDS, Session, Upstream};
use crate::ssh_proxy::wire::{self, Answer, Identity, Request};
use mur_common::agent::SshEntitlement;
use mur_common::ssh_proxy_refusals;

/// In-process fake ssh-agent. Records every frame it was sent.
struct FakeUpstream {
    keys: Vec<Vec<u8>>,
    up: bool,
    sent: Mutex<Vec<Vec<u8>>>,
}

impl FakeUpstream {
    fn new(keys: Vec<Vec<u8>>) -> Self {
        Self {
            keys,
            up: true,
            sent: Mutex::new(Vec::new()),
        }
    }
}

impl Upstream for FakeUpstream {
    async fn roundtrip(&self, frame: Vec<u8>) -> std::io::Result<Vec<u8>> {
        self.sent.lock().unwrap().push(frame.clone());
        if !self.up {
            return Err(std::io::Error::other("connection refused"));
        }
        let ans = match wire::decode_request(&frame[4..]).unwrap() {
            Request::RequestIdentities => Answer::Identities(
                self.keys
                    .iter()
                    .map(|k| Identity {
                        key_blob: k.clone(),
                        comment: b"c".to_vec(),
                    })
                    .collect(),
            ),
            _ => Answer::SignResponse(b"A REAL SIGNATURE".to_vec()),
        };
        Ok(ans.encode()[4..].to_vec())
    }
}

struct World {
    home: tempfile::TempDir,
    keys: Vec<Ed>,
    host: Ed,
}

impl World {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            keys: vec![Ed::new(), Ed::new(), Ed::new()],
            host: Ed::new(),
        }
    }
    fn upstream(&self) -> FakeUpstream {
        FakeUpstream::new(self.keys.iter().map(TestHost::key_blob).collect())
    }
    /// Policy allows key 0 on `host` only.
    fn policy(&self) -> Policy {
        policy_for(
            &fingerprint(&self.keys[0].key_blob()),
            &fingerprint(&self.host.key_blob()),
        )
    }
    fn sink(&self) -> RefusalSink {
        RefusalSink::new(Some(self.home.path().to_path_buf()))
    }
    fn last_code(&self) -> Option<String> {
        ssh_proxy_refusals::read(self.home.path())
            .first()
            .map(|r| r.code.clone())
    }
    fn good_bind(&self) -> Vec<u8> {
        bind_body(&self.host.key_blob(), b"sid", &self.host.sign_blob(b"sid"))
    }
}

/// Sign through a session with optional bind; return (answer, last code).
async fn sign_with(
    w: &World,
    policy: &Policy,
    up: &FakeUpstream,
    bind: Option<Vec<u8>>,
    key: usize,
) -> (Answer, Option<String>) {
    let sink = w.sink();
    let mut sess = Session::new(policy, up, &sink);
    if let Some(b) = bind {
        sess.handle(&b).await;
    }
    let a = sess.handle(&sign_body(&w.keys[key].key_blob())).await;
    (a, w.last_code())
}

#[tokio::test]
async fn listing_shows_only_policy_keys() {
    let w = World::new();
    let (p, up, sink) = (w.policy(), w.upstream(), w.sink());
    let mut sess = Session::new(&p, &up, &sink);
    let Answer::Identities(ids) = sess.handle(&[wire::SSH_AGENTC_REQUEST_IDENTITIES]).await else {
        panic!("expected identities");
    };
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].key_blob, w.keys[0].key_blob());
}

#[tokio::test]
async fn allowed_pair_is_still_refused_hitl_not_wired_and_never_forwarded() {
    let w = World::new();
    let (p, up) = (w.policy(), w.upstream());
    let (a, code) = sign_with(&w, &p, &up, Some(w.good_bind()), 0).await;
    assert_eq!(a, Answer::Failure);
    assert_eq!(code.as_deref(), Some("hitl_not_wired"));
    // Phase 2 invariant: no sign request reached the upstream.
    let sent = up.sent.lock().unwrap();
    assert!(
        sent.iter().all(|f| f[4] != wire::SSH_AGENTC_SIGN_REQUEST),
        "a sign request was forwarded upstream"
    );
}

#[tokio::test]
async fn key_not_allowed() {
    let w = World::new();
    let (p, up) = (w.policy(), w.upstream());
    let (a, code) = sign_with(&w, &p, &up, Some(w.good_bind()), 1).await;
    assert_eq!(
        (a, code.as_deref()),
        (Answer::Failure, Some("key_not_allowed"))
    );
}

#[tokio::test]
async fn host_not_allowed() {
    let w = World::new();
    let (p, up) = (w.policy(), w.upstream());
    let stranger = Ed::new();
    let bind = bind_body(&stranger.key_blob(), b"sid", &stranger.sign_blob(b"sid"));
    let (_, code) = sign_with(&w, &p, &up, Some(bind), 0).await;
    assert_eq!(code.as_deref(), Some("host_not_allowed"));
}

#[tokio::test]
async fn no_session_bind() {
    let w = World::new();
    let (p, up) = (w.policy(), w.upstream());
    let (_, code) = sign_with(&w, &p, &up, None, 0).await;
    assert_eq!(code.as_deref(), Some("no_session_bind"));
}

#[tokio::test]
async fn tampered_bind_is_session_bind_unverified_and_sticky() {
    let w = World::new();
    let (p, up, sink) = (w.policy(), w.upstream(), w.sink());
    let mut sig = w.host.sign_blob(b"sid");
    let last = sig.len() - 1;
    sig[last] ^= 1;
    let mut sess = Session::new(&p, &up, &sink);
    let bad = bind_body(&w.host.key_blob(), b"sid", &sig);
    assert_eq!(sess.handle(&bad).await, Answer::Failure);
    // A later valid bind must not clear the failed one.
    assert_eq!(sess.handle(&w.good_bind()).await, Answer::Failure);
    sess.handle(&sign_body(&w.keys[0].key_blob())).await;
    assert_eq!(w.last_code().as_deref(), Some("session_bind_unverified"));
}

/// A malformed session-bind (truncated, or trailing bytes) must poison the
/// connection exactly like one that fails to verify: it is still a bind
/// attempt: it must invalidate an earlier verified bind and prevent a later
/// valid bind from recovering the connection.
#[tokio::test]
async fn malformed_bind_after_a_valid_one_poisons_the_connection() {
    let w = World::new();
    let good = w.good_bind();
    for bad in [
        good[..good.len() - 3].to_vec(),
        [good.clone(), vec![0]].concat(),
    ] {
        let (p, up, sink) = (w.policy(), w.upstream(), w.sink());
        let mut sess = Session::new(&p, &up, &sink);
        assert_eq!(sess.handle(&good).await, Answer::Success);
        assert_eq!(sess.handle(&bad).await, Answer::Failure);
        sess.handle(&sign_body(&w.keys[0].key_blob())).await;
        assert_eq!(w.last_code().as_deref(), Some("session_bind_unverified"));
    }
}

#[tokio::test]
async fn malformed_first_bind_blocks_a_later_valid_one() {
    let w = World::new();
    let good = w.good_bind();
    for bad in [
        good[..good.len() - 3].to_vec(),
        [good.clone(), vec![0]].concat(),
        vec![wire::SSH_AGENTC_EXTENSION],
    ] {
        let (p, up, sink) = (w.policy(), w.upstream(), w.sink());
        let mut sess = Session::new(&p, &up, &sink);
        assert_eq!(sess.handle(&bad).await, Answer::Failure);
        assert_eq!(sess.handle(&good).await, Answer::Failure);
        sess.handle(&sign_body(&w.keys[0].key_blob())).await;
        assert_eq!(w.last_code().as_deref(), Some("session_bind_unverified"));
    }
}

#[tokio::test]
async fn too_many_binds_poison_the_connection() {
    let w = World::new();
    let (p, up, sink) = (w.policy(), w.upstream(), w.sink());
    let mut sess = Session::new(&p, &up, &sink);
    for _ in 0..MAX_SESSION_BINDS {
        assert_eq!(sess.handle(&w.good_bind()).await, Answer::Success);
    }
    assert_eq!(sess.handle(&w.good_bind()).await, Answer::Failure);
    sess.handle(&sign_body(&w.keys[0].key_blob())).await;
    assert_eq!(w.last_code().as_deref(), Some("session_bind_unverified"));
}

#[tokio::test]
async fn upstream_unavailable() {
    let w = World::new();
    let p = w.policy();
    let mut up = w.upstream();
    up.up = false;
    let sink = w.sink();
    let mut sess = Session::new(&p, &up, &sink);
    assert_eq!(
        sess.handle(&[wire::SSH_AGENTC_REQUEST_IDENTITIES]).await,
        Answer::Failure
    );
    assert_eq!(w.last_code().as_deref(), Some("upstream_unavailable"));
}

#[tokio::test]
async fn malformed_request() {
    let w = World::new();
    let (p, up, sink) = (w.policy(), w.upstream(), w.sink());
    let mut sess = Session::new(&p, &up, &sink);
    // ADD_IDENTITY: nobody may load keys through MUR.
    assert_eq!(sess.handle(&[17, 0, 0, 0, 0]).await, Answer::Failure);
    assert_eq!(w.last_code().as_deref(), Some("malformed_request"));
}

#[tokio::test]
async fn oversize_frame_ends_the_connection_as_malformed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let w = World::new();
    let (p, up, sink) = (w.policy(), w.upstream(), w.sink());
    let (mut client, server) = tokio::io::duplex(64);
    let serve = Session::new(&p, &up, &sink).serve(server);
    let drive = async {
        client
            .write_all(&(wire::MAX_FRAME_BYTES as u32 + 1).to_be_bytes())
            .await
            .unwrap();
        let mut out = Vec::new();
        client.read_to_end(&mut out).await.unwrap();
        out
    };
    let (res, out) = tokio::join!(serve, drive);
    res.unwrap();
    assert_eq!(out, Answer::Failure.encode());
    assert_eq!(w.last_code().as_deref(), Some("malformed_request"));
}

#[tokio::test]
async fn policy_invalid_and_unpinned_refuse_everything_and_list_nothing() {
    for (policy, want) in [
        (policy_for("not-a-fingerprint", "x"), "policy_invalid"),
        (
            Policy::build(&SshEntitlement::default(), PinTrust::Unpinned, true),
            "policy_unpinned",
        ),
    ] {
        let w = World::new();
        let up = w.upstream();
        let (_, code) = sign_with(&w, &policy, &up, Some(w.good_bind()), 0).await;
        assert_eq!(code.as_deref(), Some(want));
        let sink = w.sink();
        let mut sess = Session::new(&policy, &up, &sink);
        assert_eq!(
            sess.handle(&[wire::SSH_AGENTC_REQUEST_IDENTITIES]).await,
            Answer::Identities(vec![])
        );
        assert!(
            up.sent.lock().unwrap().is_empty(),
            "{want}: upstream contacted"
        );
    }
}

/// `hitl_unavailable` needs the Phase 3 gate; Phase 2 can only show the
/// code exists and renders. Kept here so the "each code has a test" list is
/// checkable in one file.
#[test]
fn hitl_unavailable_is_reserved_for_phase_3() {
    let m = crate::ssh_proxy::refusal::Refusal::new(RefusalCode::HitlUnavailable).message();
    assert!(m.contains("hitl_unavailable") && m.contains("never passed through"));
}
