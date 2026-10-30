//! Listener tests (plan Task 2.3, "Phase 2 tests"): a real unix socket in
//! front of a fake upstream ssh-agent on a real unix socket, so the held
//! stream, the peer gate and close-on-exec are the production code paths.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use super::fixtures::*;
use super::gate::{FakeGate, NeverAsked};
use super::policy_for;
use crate::ssh_proxy::gate::{GateOutcome, SignGate};
use crate::ssh_proxy::keys::fingerprint;
use crate::ssh_proxy::listener::{self, HeldUpstream, PeerGate, Proxy};
use crate::ssh_proxy::refusal::RefusalSink;
use crate::ssh_proxy::wire::{self, Answer, Identity, Request};
use mur_common::ssh_proxy_refusals;

/// Fake upstream agent on a real socket. Counts every byte it receives.
struct FakeAgent {
    path: std::path::PathBuf,
    bytes_in: Arc<AtomicUsize>,
    _dir: tempfile::TempDir,
}

impl FakeAgent {
    fn start(keys: Vec<Vec<u8>>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("up.sock");
        let l = UnixListener::bind(&path).unwrap();
        let bytes_in = Arc::new(AtomicUsize::new(0));
        let counter = bytes_in.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let keys = keys.clone();
                let counter = counter.clone();
                tokio::spawn(async move {
                    while let Ok(Some(Ok(body))) = wire::read_frame(&mut s).await {
                        counter.fetch_add(body.len() + 4, Ordering::SeqCst);
                        let ans = match wire::decode_request(&body) {
                            Ok(Request::RequestIdentities) => Answer::Identities(
                                keys.iter()
                                    .map(|k| Identity {
                                        key_blob: k.clone(),
                                        comment: b"c".to_vec(),
                                    })
                                    .collect(),
                            ),
                            _ => Answer::SignResponse(b"A REAL SIGNATURE".to_vec()),
                        };
                        if s.write_all(&ans.encode()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self {
            path,
            bytes_in,
            _dir: dir,
        }
    }
}

struct Rig {
    home: tempfile::TempDir,
    keys: Vec<Ed>,
    host: Ed,
    agent: FakeAgent,
    sock: std::path::PathBuf,
    upstream: Arc<HeldUpstream>,
}

async fn rig(gate: PeerGate) -> Rig {
    rig_with(gate, Arc::new(NeverAsked)).await
}

async fn rig_with(gate: PeerGate, approvals: Arc<dyn SignGate>) -> Rig {
    let keys = vec![Ed::new(), Ed::new(), Ed::new()];
    let host = Ed::new();
    let agent = FakeAgent::start(keys.iter().map(TestHost::key_blob).collect());
    let home = tempfile::tempdir().unwrap();
    let upstream = Arc::new(HeldUpstream::connect(&agent.path).await.unwrap());
    let sock = listener::socket_path(home.path());
    let l = listener::bind(&sock).unwrap();
    let proxy = Arc::new(Proxy {
        policy: policy_for(
            &fingerprint(&keys[0].key_blob()),
            &fingerprint(&host.key_blob()),
        ),
        upstream: upstream.clone(),
        sink: RefusalSink::new(Some(home.path().to_path_buf())),
        gate,
        approvals,
    });
    tokio::spawn(listener::serve(l, proxy));
    Rig {
        home,
        keys,
        host,
        agent,
        sock,
        upstream,
    }
}

fn admit_all() -> PeerGate {
    Arc::new(|_| Ok(()))
}

async fn ask(s: &mut UnixStream, body: &[u8]) -> Answer {
    let mut f = Vec::new();
    wire::put_string(&mut f, body);
    s.write_all(&f).await.unwrap();
    let got = wire::read_frame(s).await.unwrap().unwrap().unwrap();
    wire::decode_answer(&got).unwrap()
}

fn last_code(r: &Rig) -> Option<String> {
    ssh_proxy_refusals::read(r.home.path())
        .first()
        .map(|x| x.code.clone())
}

#[tokio::test]
async fn listing_through_the_socket_shows_only_the_policy_key() {
    let r = rig(admit_all()).await;
    let mut c = UnixStream::connect(&r.sock).await.unwrap();
    let Answer::Identities(ids) = ask(&mut c, &[wire::SSH_AGENTC_REQUEST_IDENTITIES]).await else {
        panic!("expected an identities answer");
    };
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].key_blob, r.keys[0].key_blob());
}

#[tokio::test]
async fn an_approved_sign_returns_the_upstream_signature() {
    let r = rig_with(admit_all(), FakeGate::new(GateOutcome::Allow)).await;
    let mut c = UnixStream::connect(&r.sock).await.unwrap();
    let bind = bind_body(&r.host.key_blob(), b"sid", &r.host.sign_blob(b"sid"));
    assert_eq!(ask(&mut c, &bind).await, Answer::Success);
    let a = ask(&mut c, &sign_body(&r.keys[0].key_blob())).await;
    assert_eq!(a, Answer::SignResponse(b"A REAL SIGNATURE".to_vec()));
    assert_eq!(last_code(&r), None);
}

#[tokio::test]
async fn a_denied_sign_is_never_forwarded() {
    let r = rig_with(admit_all(), FakeGate::new(GateOutcome::Deny)).await;
    let mut c = UnixStream::connect(&r.sock).await.unwrap();
    let bind = bind_body(&r.host.key_blob(), b"sid", &r.host.sign_blob(b"sid"));
    assert_eq!(ask(&mut c, &bind).await, Answer::Success);
    let before = r.agent.bytes_in.load(Ordering::SeqCst);
    let a = ask(&mut c, &sign_body(&r.keys[0].key_blob())).await;
    assert_eq!(a, Answer::Failure);
    assert_eq!(last_code(&r).as_deref(), Some("hitl_denied"));
    assert_eq!(
        r.agent.bytes_in.load(Ordering::SeqCst),
        before,
        "a denied sign request must never reach the upstream"
    );
}

#[tokio::test]
async fn foreign_peer_is_closed_with_zero_bytes_forwarded() {
    let r = rig(Arc::new(|_| Err("not this agent".into()))).await;
    let mut c = UnixStream::connect(&r.sock).await.unwrap();
    // The client speaks first; the proxy must not read or forward it.
    let mut f = Vec::new();
    wire::put_string(&mut f, &[wire::SSH_AGENTC_REQUEST_IDENTITIES]);
    let _ = c.write_all(&f).await;
    let mut buf = Vec::new();
    let n = c.read_to_end(&mut buf).await.unwrap_or(0);
    assert_eq!(n, 0, "a refused peer gets no answer, just a close");
    assert_eq!(last_code(&r).as_deref(), Some("peer_not_self"));
    assert_eq!(r.agent.bytes_in.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn self_only_gate_admits_only_this_process_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let gate = listener::self_only_gate(std::process::id(), dir.path().to_path_buf());
    assert!(gate(Some(std::process::id())).is_ok());
    assert!(gate(None).is_err(), "an unidentified peer is refused");
    // pid 1 is never a descendant of this test process.
    assert!(gate(Some(1)).is_err());
}

#[tokio::test]
async fn upstream_that_was_never_reached_refuses_upstream_unavailable() {
    let home = tempfile::tempdir().unwrap();
    let k = Ed::new();
    let sock = listener::socket_path(home.path());
    let proxy = Arc::new(Proxy {
        policy: policy_for(
            &fingerprint(&k.key_blob()),
            &fingerprint(&Ed::new().key_blob()),
        ),
        upstream: Arc::new(HeldUpstream::disconnected()),
        sink: RefusalSink::new(Some(home.path().to_path_buf())),
        gate: admit_all(),
        approvals: Arc::new(NeverAsked),
    });
    tokio::spawn(listener::serve(listener::bind(&sock).unwrap(), proxy));
    let mut c = UnixStream::connect(&sock).await.unwrap();
    assert_eq!(
        ask(&mut c, &[wire::SSH_AGENTC_REQUEST_IDENTITIES]).await,
        Answer::Failure
    );
    let code = ssh_proxy_refusals::read(home.path())
        .first()
        .map(|x| x.code.clone());
    assert_eq!(code.as_deref(), Some("upstream_unavailable"));
}

#[tokio::test]
async fn a_dropped_upstream_is_not_reconnected() {
    // An upstream that answers nothing and closes at once.
    let dir = tempfile::tempdir().unwrap();
    let up = dir.path().join("up.sock");
    let l = UnixListener::bind(&up).unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            drop(s);
        }
    });
    let held = HeldUpstream::connect(&up).await.unwrap();
    use crate::ssh_proxy::session::Upstream;
    assert!(
        held.roundtrip(Request::RequestIdentities.encode())
            .await
            .is_err()
    );
    assert!(
        !held.is_connected().await,
        "a failed exchange drops the stream"
    );
    assert!(
        held.roundtrip(Request::RequestIdentities.encode())
            .await
            .is_err(),
        "and nothing reconnects it"
    );
}

#[tokio::test]
async fn missing_upstream_keys_names_listed_keys_the_agent_lacks() {
    let r = rig(admit_all()).await;
    let absent = Ed::new();
    let p = policy_for(
        &fingerprint(&absent.key_blob()),
        &fingerprint(&r.host.key_blob()),
    );
    assert_eq!(
        listener::missing_upstream_keys(&p, &r.upstream).await,
        Some(vec![fingerprint(&absent.key_blob())])
    );
    let held = policy_for(
        &fingerprint(&r.keys[1].key_blob()),
        &fingerprint(&r.host.key_blob()),
    );
    assert_eq!(
        listener::missing_upstream_keys(&held, &r.upstream).await,
        Some(vec![])
    );
}

#[test]
fn socket_is_user_only_and_a_stale_one_is_replaced() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let p = listener::socket_path(home.path());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        drop(listener::bind(&p).unwrap());
        // A socket file left behind by an earlier run is replaced.
        let _l = listener::bind(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    });
    // A regular file in the way is not deleted.
    let f = home.path().join("not-a-socket");
    std::fs::write(&f, b"x").unwrap();
    rt.block_on(async { assert!(listener::bind(&f).is_err()) });
    assert!(f.exists());
}

#[test]
fn over_long_path_does_not_fit() {
    let long = std::path::PathBuf::from(format!("/{}", "a".repeat(200)));
    assert!(!listener::socket_path_fits(
        &long.join(listener::SSH_PROXY_SOCKET_NAME)
    ));
    assert!(listener::socket_path_fits(std::path::Path::new(
        "/h/ssh-agent.sock"
    )));
}

/// Listener and held upstream are close-on-exec: a bash child holds neither.
#[tokio::test]
async fn bash_child_holds_neither_listener_nor_upstream() {
    use std::os::fd::AsRawFd;
    let r = rig(admit_all()).await;
    let up_fd = r.upstream.raw_fd().await.unwrap();
    let l = UnixListener::bind(r.home.path().join("second.sock")).unwrap();
    let l_fd = l.as_raw_fd();
    // Both flags first: the property the child result rests on.
    for fd in [up_fd, l_fd] {
        // SAFETY: F_GETFD on an fd this test owns.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(
            flags >= 0 && flags & libc::FD_CLOEXEC != 0,
            "fd {fd} not CLOEXEC"
        );
    }
    // `[ -e /dev/fd/N ]` stats without redirecting: a `>&N` probe would see
    // bash's own saved-stdout copy, which bash parks at fd 10 and up.
    // Control: an inheritable dup must read `open`, so `closed` is observed.
    // SAFETY: dup of an fd this test owns; the copy is closed below.
    let ctl = unsafe { libc::dup(l_fd) };
    assert!(ctl >= 0 && unsafe { libc::fcntl(ctl, libc::F_GETFD) } & libc::FD_CLOEXEC == 0);
    let script = format!(
        "for f in {up_fd} {l_fd} {ctl}; do if [ -e /dev/fd/$f ]; then echo open:$f; else echo closed:$f; fi; done"
    );
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(script)
        .output()
        .unwrap();
    // SAFETY: closing the control dup made above.
    unsafe { libc::close(ctl) };
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        s.trim(),
        format!("closed:{up_fd}\nclosed:{l_fd}\nopen:{ctl}"),
        "{s}"
    );
}
