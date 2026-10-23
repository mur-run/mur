#![cfg(unix)]
//! A private remote behind HTTP Basic auth, served by the real `git-http-backend`, driven through
//! the broker from `submit_request` to `on_approval`. Plain loopback http (the `test-support`
//! endpoint seam): git's 401 -> askpass -> retry flow is the same with or without TLS.
//!
//! The question this file answers: does a credential-bound enrollment get past *intake*? Intake
//! prefetches the target ref from the remote before any human has approved anything, so a remote
//! that refuses anonymous reads must be readable at that point too, or the request never reaches
//! approval at all.
use mur_git_broker::credential::{Credential, SecretPath, Username};
use mur_git_broker::credential_source::CredentialSource;
use mur_git_broker::{broker::*, error::BrokerError, import::RlimitSpawn, pending::*};
use mur_git_broker::{policy::BrokerLimits, prefetch::*, push::NoAuth};
use secrecy::SecretString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
mod common;
use common::orch::*;
use common::*;

const TOKEN: &str = "s3cr3t-token-value";
const USER: &str = "bot";

struct Fixed(String);
const WRONG: &str = "wrong-token-value";
impl CredentialSource for Fixed {
    fn open(&self, _: &SecretPath) -> Result<SecretString, BrokerError> {
        Ok(SecretString::from(self.0.clone()))
    }
}

/// What the server saw: the request line and whether the right Basic credential came with it.
#[derive(Debug, Clone)]
struct Seen {
    #[allow(dead_code)] // shown in failure messages
    line: String,
    authed: bool,
    leaked_token_in_line: bool,
}

/// Serves every repo under `root` through `git-http-backend`. With `require_auth`, a request
/// without `Authorization: Basic base64(bot:TOKEN)` is answered `401` and never reaches the
/// backend.
fn serve(root: PathBuf, require_auth: bool) -> (u16, Arc<Mutex<Vec<Seen>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let want = format!("Basic {}", b64(format!("{USER}:{TOKEN}").as_bytes()));
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(s) = conn else { return };
            let (root, log, want) = (root.clone(), log.clone(), want.clone());
            std::thread::spawn(move || handle(s, &root, require_auth, &want, &log));
        }
    });
    (port, seen)
}

fn handle(s: TcpStream, root: &Path, require_auth: bool, want: &str, log: &Mutex<Vec<Seen>>) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).is_err() {
        return;
    }
    let line = line.trim_end().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.trim_end().split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
    }
    let header = |k: &str| headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert!(
        header("transfer-encoding").is_none(),
        "chunked request bodies are not handled by this test server"
    );
    let authed = header("authorization").as_deref() == Some(want);
    log.lock().unwrap().push(Seen {
        line: line.clone(),
        authed,
        leaked_token_in_line: line.contains(TOKEN),
    });
    let mut out = s;
    if require_auth && !authed {
        let _ = out.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"t\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    }
    let len: usize = header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).unwrap();

    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or("GET").to_owned();
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let backend = PathBuf::from(git(root, &["--exec-path"]).trim()).join("git-http-backend");
    let mut cmd = Command::new(backend);
    cmd.env_clear()
        .env("GATEWAY_INTERFACE", "CGI/1.1")
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", &method)
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("REMOTE_ADDR", "127.0.0.1")
        // receive-pack is enabled only for an authenticated user.
        .env("REMOTE_USER", USER)
        .env("CONTENT_LENGTH", len.to_string());
    for (k, v) in [
        ("content-type", "CONTENT_TYPE"),
        ("content-encoding", "HTTP_CONTENT_ENCODING"),
        ("git-protocol", "GIT_PROTOCOL"),
    ] {
        if let Some(val) = header(k) {
            cmd.env(v, val);
        }
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&body).unwrap();
    let cgi = child.wait_with_output().unwrap().stdout;
    let split = cgi
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    let split = split.or_else(|| cgi.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)));
    let (head, rest) = match split {
        Some((i, n)) => (
            String::from_utf8_lossy(&cgi[..i]).into_owned(),
            &cgi[i + n..],
        ),
        None => (String::new(), &cgi[..]),
    };
    let mut status = "200 OK".to_owned();
    let mut resp = String::new();
    for h in head.lines() {
        match h.split_once(':') {
            Some((k, v)) if k.eq_ignore_ascii_case("status") => status = v.trim().to_owned(),
            Some(_) => resp.push_str(&format!("{}\r\n", h.trim_end())),
            None => {}
        }
    }
    let _ = write!(
        out,
        "HTTP/1.1 {status}\r\n{resp}Content-Length: {}\r\nConnection: close\r\n\r\n",
        rest.len()
    );
    let _ = out.write_all(rest);
}

fn b64(input: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() {
                T[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

struct Rig {
    e: Env,
    b: Broker,
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// The daemon's wiring (`open_broker`), with the enrollment's own URL handed to the reader, on a
/// credential-bound enrollment of `http://127.0.0.1:<port>/remote.git`.
fn rig(require_auth: bool) -> Rig {
    rig_with(require_auth, TOKEN, None)
}

/// `enrolled`: the URL the enrollment binds, when it is not the git server itself.
fn rig_with(require_auth: bool, secret: &str, enrolled: Option<String>) -> Rig {
    let e = env();
    let (port, seen) = serve(e.t.path().to_path_buf(), require_auth);
    let url = enrolled.unwrap_or_else(|| format!("http://127.0.0.1:{port}/remote.git"));
    let mut p = policy();
    p.canonical_remote_endpoint = url.clone();
    let mut records = records_for(&url, &p);
    records.enrollments[0].credential = Some(Credential {
        secret_path: SecretPath::parse("/secrets/deploy-token").unwrap(),
        username: Username::parse(USER).unwrap(),
    });
    *e.records.0.lock().unwrap() = Ok(Some(records));
    let b = Broker::new(BrokerConfig {
        store_path: e.t.path().join("pending.sqlite"),
        work_root: e.t.path().join("work-root"),
        limits: BrokerLimits::default(),
        git_bin: git_bin(),
        enrollments: Box::new(e.records.clone()),
        reader: Box::new(|url: &str| {
            Box::new(GitFetchReader {
                remote_url: url.to_string(),
                git_bin: git_bin(),
            })
        }),
        spawn: Box::new(RlimitSpawn),
        auth: Box::new(NoAuth),
        askpass_bin: PathBuf::from(env!("CARGO_BIN_EXE_askpass-probe")),
        credentials: Box::new(Fixed(secret.into())),
        clock: e.clock.clone(),
    })
    .unwrap();
    Rig { e, b, url, seen }
}

fn submit(r: &Rig) -> Result<(RequestKey, String), BrokerError> {
    let mut d = action_for(&r.e.old, &r.e.new, "refs/heads/agent/x");
    d.request_id = "r1".into();
    let p =
        r.e.records
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .enrollments[0]
            .policy();
    d.canonical_remote_endpoint = r.url.clone();
    d.remote_policy_digest = p.remote_digest();
    d.ref_policy_digest = p.digest();
    d.enrollment_epoch = EPOCH;
    d.repo_identity = IDENTITY.into();
    let pack = range_pack(&r.e.work, &r.e.new, Some(&r.e.old), false);
    let k = key("r1");
    r.b.submit_request(&k, &d, &pack).map(|h| (k, h))
}

fn remote_ref(r: &Rig) -> String {
    git(&r.e.remote, &["rev-parse", "refs/heads/agent/x"])
        .trim()
        .to_owned()
}

/// Control: the harness itself works. With anonymous reads allowed, intake gets to approval.
#[test]
fn an_anonymous_readable_remote_reaches_approval() {
    let r = rig(false);
    submit(&r).expect("intake succeeds when the remote allows anonymous reads");
}

/// The private-repo path end to end: intake must read the remote with the enrolled credential,
/// the push after approval must land, and the token must never travel in a request line.
#[test]
fn a_private_remote_is_prefetched_with_the_credential_and_the_push_lands() {
    let r = rig(true);
    let (k, h) = match submit(&r) {
        Ok(v) => v,
        Err(e) => panic!(
            "intake failed on a credential-bound private remote: {e:?}; server saw {:#?}",
            r.seen.lock().unwrap()
        ),
    };
    assert_eq!(
        remote_ref(&r),
        r.e.old,
        "nothing is written before approval"
    );
    assert_eq!(
        r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded
    );
    assert_eq!(remote_ref(&r), r.e.new, "the push moved the remote ref");
    let seen = r.seen.lock().unwrap();
    assert!(seen.iter().any(|s| s.authed), "{seen:#?}");
    assert!(seen.iter().all(|s| !s.leaked_token_in_line), "{seen:#?}");
}

/// Answers every request `302` to `to` and records whether it carried any `Authorization`.
fn redirector(to: String) -> (u16, Arc<Mutex<Vec<bool>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { return };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut auth = false;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
                auth |= h.to_ascii_lowercase().starts_with("authorization:");
            }
            log.lock().unwrap().push(auth);
            let _ = write!(
                s,
                "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    (port, seen)
}

/// A public remote never asks, so git never runs the askpass and the token is never sent, even
/// though the enrollment carries one.
#[test]
fn an_anonymous_readable_remote_is_never_sent_the_token() {
    let r = rig(false);
    let (k, h) = submit(&r).unwrap();
    r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap();
    assert_eq!(remote_ref(&r), r.e.new);
    let seen = r.seen.lock().unwrap();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|s| !s.authed), "{seen:#?}");
}

/// The prefetch runs before approval, so a redirect there must not carry the token anywhere:
/// git is told not to follow it, and the request fails closed with no pending row.
#[test]
fn a_prefetch_redirect_is_not_followed_and_never_carries_the_token() {
    let t = tempfile::tempdir().unwrap();
    let (evil_port, evil_seen) = serve(t.path().to_path_buf(), false);
    let (port, redirect_seen) = redirector(format!(
        "http://127.0.0.1:{evil_port}/remote.git/info/refs?service=git-upload-pack"
    ));
    let r = rig_with(
        true,
        TOKEN,
        Some(format!("http://127.0.0.1:{port}/remote.git")),
    );
    let err = submit(&r).unwrap_err();
    assert!(matches!(err, BrokerError::PrefetchRejected(_)), "{err:?}");
    assert!(
        r.b.store().get(&key("r1")).unwrap().is_none(),
        "no pending row"
    );
    assert!(
        evil_seen.lock().unwrap().is_empty(),
        "the redirect was followed"
    );
    assert!(
        redirect_seen.lock().unwrap().iter().all(|a| !a),
        "token sent unprompted"
    );
}

/// A wrong credential is refused by the remote; the error names no token and leaves no row.
#[test]
fn a_rejected_credential_fails_intake_without_echoing_the_token() {
    let r = rig_with(true, WRONG, None);
    let err = submit(&r).unwrap_err();
    assert!(matches!(err, BrokerError::PrefetchRejected(_)), "{err:?}");
    let shown = format!("{err:?} {err}");
    assert!(!shown.contains(WRONG) && !shown.contains(TOKEN), "{shown}");
    assert!(r.b.store().get(&key("r1")).unwrap().is_none());
    assert_eq!(remote_ref(&r), r.e.old);
}

/// Handing a credential to the pre-approval prefetch is audited under its own kind, apart from
/// the push.
#[test]
fn a_credentialed_prefetch_is_audited_as_such() {
    let r = rig(true);
    submit(&r).unwrap();
    assert_eq!(
        r.b.store()
            .audit_count(
                "alice",
                mur_git_broker::broker::AUDIT_PREFETCH_WITH_CREDENTIAL
            )
            .unwrap(),
        1
    );
}
