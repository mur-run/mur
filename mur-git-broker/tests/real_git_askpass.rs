#![cfg(unix)]
//! Real git, real `GIT_ASKPASS` binary, real fd 57 — against a local plain-http server. TLS is
//! left out on purpose: git's credential flow (401 -> askpass -> retry) is the same either way,
//! and `TokenAuth::new_plain_http_for_test` differs from `new` only in the scheme it accepts.
//!
//! Complements `https_askpass.rs`, whose stub git proves what `apply` *sets*; this proves what a
//! real git *does with it*.
use mur_git_broker::askpass::TokenAuth;
use mur_git_broker::git::{GitRunner, run_with_timeout};
use mur_git_broker::oid::ObjectFormat;
use mur_git_broker::push::RemoteAuth;
use secrecy::SecretString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
mod common;
use common::git_bin;

const TOKEN: &str = "s3cr3t-token-value";
const USER: &str = "bot";

/// One request as the server saw it.
#[derive(Debug, Clone)]
struct Seen {
    request_line: String,
    authorization: Option<String>,
}

/// Answers `401 Basic` until an `Authorization` header arrives, then serves an empty repo.
/// With `redirect_to`, every request is answered `302` to that URL instead.
struct Server {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn serve(redirect_to: Option<String>) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { return };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let text = String::from_utf8_lossy(&buf).into_owned();
            let mut lines = text.split("\r\n");
            let request_line = lines.next().unwrap_or("").to_owned();
            let authorization = lines
                .find_map(|l| l.strip_prefix("Authorization: "))
                .map(str::to_owned);
            log.lock().unwrap().push(Seen {
                request_line,
                authorization: authorization.clone(),
            });
            let reply = if let Some(to) = &redirect_to {
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            } else if authorization.is_none() {
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"t\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
            } else {
                let body = "001e# service=git-upload-pack\n00000000";
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            let _ = s.write_all(reply.as_bytes());
        }
    });
    Server { addr, seen }
}

fn b64(input: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

struct Run {
    code: i32,
    stderr: String,
    stdout: String,
}

/// `git ls-remote <url>` through the broker's own scrubbed command + `TokenAuth::apply`.
fn ls_remote(url: &str, secret: &str) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("r.git");
    GitRunner::init_bare(&git_bin(), &repo, ObjectFormat::Sha1).unwrap();
    let askpass = PathBuf::from(env!("CARGO_BIN_EXE_askpass-probe"));
    let auth = TokenAuth::new_plain_http_for_test(
        SecretString::from(secret.to_owned()),
        USER,
        url,
        askpass,
    )
    .expect("plain-http url is accepted");
    let mut cmd = GitRunner::new(git_bin(), repo).command(&["ls-remote", url]);
    auth.apply(&mut cmd);
    let out = run_with_timeout(cmd, Duration::from_secs(30)).unwrap();
    Run {
        code: out.code,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

#[test]
fn real_git_runs_the_real_askpass_and_sends_the_token_as_basic_auth() {
    let srv = serve(None);
    let url = format!("http://{}/org/repo.git", srv.addr);
    let r = ls_remote(&url, TOKEN);
    assert_eq!(r.code, 0, "stderr: {}\nstdout: {}", r.stderr, r.stdout);
    let seen = srv.seen.lock().unwrap().clone();
    assert!(seen.len() >= 2, "{seen:?}");
    assert!(
        seen.iter()
            .all(|s| s.request_line.contains("/org/repo.git/info/refs")),
        "only the bound repo is contacted: {seen:?}"
    );
    assert!(
        seen[0].authorization.is_none(),
        "first request is anonymous: {seen:?}"
    );
    let want = format!("Basic {}", b64(format!("{USER}:{TOKEN}").as_bytes()));
    let authed: Vec<_> = seen
        .iter()
        .filter_map(|s| s.authorization.as_ref())
        .collect();
    assert!(
        !authed.is_empty() && authed.iter().all(|a| **a == want),
        "{seen:?}"
    );
    assert!(!r.stderr.contains(TOKEN) && !r.stdout.contains(TOKEN));
}

#[test]
fn a_redirect_is_not_followed_so_the_token_never_reaches_another_host() {
    let other = serve(None);
    let srv = serve(Some(format!("http://{}/steal", other.addr)));
    let url = format!("http://{}/org/repo.git", srv.addr);
    let r = ls_remote(&url, TOKEN);
    assert_ne!(r.code, 0, "a redirect must fail the command");
    assert!(
        other.seen.lock().unwrap().is_empty(),
        "the redirect target must not be contacted: {:?}",
        other.seen.lock().unwrap()
    );
    assert!(
        srv.seen
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.authorization.is_none())
    );
}

#[test]
fn a_wrong_username_is_never_answered() {
    // The prompt names a different origin than the one bound, so askpass declines.
    let srv = serve(None);
    let url = format!("http://{}/org/repo.git", srv.addr);
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("r.git");
    GitRunner::init_bare(&git_bin(), &repo, ObjectFormat::Sha1).unwrap();
    let askpass = PathBuf::from(env!("CARGO_BIN_EXE_askpass-probe"));
    let auth = TokenAuth::new_plain_http_for_test(
        SecretString::from(TOKEN.to_owned()),
        USER,
        &url,
        askpass,
    )
    .unwrap();
    let mut cmd = GitRunner::new(git_bin(), repo).command(&["ls-remote", &url]);
    auth.apply(&mut cmd);
    // Override the username git will put in the prompt, so it no longer matches MUR_ASKPASS_ORIGIN.
    cmd.env("GIT_CONFIG_VALUE_0", "mallory");
    let out = run_with_timeout(cmd, Duration::from_secs(30)).unwrap();
    assert_ne!(out.code, 0);
    assert!(
        srv.seen
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.authorization.is_none())
    );
}
