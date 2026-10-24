#![allow(dead_code)]
//! A private remote behind HTTP Basic auth, served by the real `git-http-backend` on loopback,
//! plus the broker rig that drives it. Shared by `private_http_e2e.rs` and its split-credential
//! sibling.
use crate::common::orch::*;
use crate::common::*;
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
/// The one credential of a single-credential enrollment: `bot:TOKEN` for both phases.
pub const TOKEN: &str = "s3cr3t-token-value";
pub const USER: &str = "bot";
pub const WRONG: &str = "wrong-token-value";
/// A split enrollment: `reader:READ_TOKEN` prefetches, `bot:WRITE_TOKEN` pushes.
pub const READ_TOKEN: &str = "r3ad-only-token-value";
pub const WRITE_TOKEN: &str = "wr1te-only-token-value";
pub const READER: &str = "reader";
pub const WRITE_PATH: &str = "/secrets/deploy-token";
pub const READ_PATH: &str = "/secrets/read-token";

/// Hands out the secret keyed by the path a credential points at, like the real source does.
pub struct Keyed(Vec<(&'static str, String)>);
impl CredentialSource for Keyed {
    fn open(&self, p: &SecretPath) -> Result<SecretString, BrokerError> {
        self.0
            .iter()
            .find(|(path, _)| *path == p.as_str())
            .map(|(_, secret)| SecretString::from(secret.clone()))
            .ok_or(BrokerError::CredentialUnreadable("not_found"))
    }
}

/// Which credential a request carried, judged against the logins the server was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carried {
    Nothing,
    Read,
    Write,
    /// An `Authorization` header that is neither login.
    Wrong,
}

/// The part of the git protocol a request belongs to, told apart by URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    /// `git-upload-pack` or `info/refs?service=git-upload-pack`: the pre-approval prefetch.
    Prefetch,
    /// `git-receive-pack` or `info/refs?service=git-receive-pack`: the push.
    Push,
    Other,
}

fn leg_of(line: &str) -> Leg {
    if line.contains("git-upload-pack") {
        Leg::Prefetch
    } else if line.contains("git-receive-pack") {
        Leg::Push
    } else {
        Leg::Other
    }
}

#[derive(Clone)]
pub struct Login {
    header: String,
    token: &'static str,
}
impl Login {
    fn new(user: &str, token: &'static str) -> Self {
        let header = format!("Basic {}", b64(format!("{user}:{token}").as_bytes()));
        Self { header, token }
    }
}

/// The logins the server accepts. With no `read` login the one `write` login serves both legs.
#[derive(Clone)]
pub struct Logins {
    read: Option<Login>,
    write: Login,
}
impl Logins {
    pub fn single() -> Self {
        Self {
            read: None,
            write: Login::new(USER, TOKEN),
        }
    }
    pub fn split() -> Self {
        Self {
            read: Some(Login::new(READER, READ_TOKEN)),
            write: Login::new(USER, WRITE_TOKEN),
        }
    }
    fn classify(&self, header: Option<&str>) -> Carried {
        match header {
            None => Carried::Nothing,
            Some(h) if self.read.as_ref().is_some_and(|r| r.header == h) => Carried::Read,
            Some(h) if self.write.header == h => Carried::Write,
            Some(_) => Carried::Wrong,
        }
    }
    /// Whether `c` is the login this leg is supposed to authenticate with.
    fn accepts(&self, leg: Leg, c: Carried) -> bool {
        match leg {
            Leg::Prefetch if self.read.is_some() => c == Carried::Read,
            Leg::Prefetch | Leg::Push => c == Carried::Write,
            Leg::Other => matches!(c, Carried::Read | Carried::Write),
        }
    }
    fn leaks(&self, line: &str) -> bool {
        line.contains(self.write.token)
            || self.read.as_ref().is_some_and(|r| line.contains(r.token))
    }
}

/// What the server saw: the request line, which leg it belongs to, and which login came with it.
#[derive(Debug, Clone)]
pub struct Seen {
    #[allow(dead_code)] // shown in failure messages
    pub line: String,
    pub leg: Leg,
    pub carried: Carried,
    /// The request carried the login its leg should use.
    pub authed: bool,
    /// Either token appears in the request line.
    pub leaked_token_in_line: bool,
}

/// Serves every repo under `root` through `git-http-backend`. With `require_auth`, a request
/// whose leg does not carry its own `Authorization: Basic` login is answered `401` and never
/// reaches the backend.
pub fn serve(root: PathBuf, require_auth: bool, logins: Logins) -> (u16, Arc<Mutex<Vec<Seen>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(s) = conn else { return };
            let (root, log, logins) = (root.clone(), log.clone(), logins.clone());
            std::thread::spawn(move || handle(s, &root, require_auth, &logins, &log));
        }
    });
    (port, seen)
}

pub fn handle(
    s: TcpStream,
    root: &Path,
    require_auth: bool,
    logins: &Logins,
    log: &Mutex<Vec<Seen>>,
) {
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
    let leg = leg_of(&line);
    let carried = logins.classify(header("authorization").as_deref());
    let authed = logins.accepts(leg, carried);
    log.lock().unwrap().push(Seen {
        line: line.clone(),
        leg,
        carried,
        authed,
        leaked_token_in_line: logins.leaks(&line),
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

pub fn b64(input: &[u8]) -> String {
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

pub struct Rig {
    pub e: Env,
    pub b: Broker,
    pub url: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

/// The daemon's wiring (`open_broker`), with the enrollment's own URL handed to the reader, on a
/// credential-bound enrollment of `http://127.0.0.1:<port>/remote.git`.
pub fn rig(require_auth: bool) -> Rig {
    rig_with(require_auth, TOKEN, None)
}

/// `enrolled`: the URL the enrollment binds, when it is not the git server itself.
pub fn rig_with(require_auth: bool, secret: &str, enrolled: Option<String>) -> Rig {
    rig_core(require_auth, None, secret, enrolled)
}

/// Like [`rig_with`] on a split enrollment: `reader` opens `read_secret` for the prefetch and
/// `bot` opens `write_secret` for the push. The server accepts `reader:READ_TOKEN` for
/// `upload-pack` and `bot:WRITE_TOKEN` for `receive-pack`.
pub fn rig_split(
    require_auth: bool,
    read_secret: &str,
    write_secret: &str,
    enrolled: Option<String>,
) -> Rig {
    rig_core(require_auth, Some(read_secret), write_secret, enrolled)
}

fn cred(path: &str, user: &str) -> Credential {
    Credential {
        secret_path: SecretPath::parse(path).unwrap(),
        username: Username::parse(user).unwrap(),
    }
}

fn rig_core(
    require_auth: bool,
    read_secret: Option<&str>,
    write_secret: &str,
    enrolled: Option<String>,
) -> Rig {
    let e = env();
    let logins = if read_secret.is_some() {
        Logins::split()
    } else {
        Logins::single()
    };
    let (port, seen) = serve(e.t.path().to_path_buf(), require_auth, logins);
    let url = enrolled.unwrap_or_else(|| format!("http://127.0.0.1:{port}/remote.git"));
    let mut p = policy();
    p.canonical_remote_endpoint = url.clone();
    let mut records = records_for(&url, &p);
    records.enrollments[0].credential = Some(cred(WRITE_PATH, USER));
    records.enrollments[0].read_credential = read_secret.map(|_| cred(READ_PATH, READER));
    let mut secrets = vec![(WRITE_PATH, write_secret.to_owned())];
    secrets.extend(read_secret.map(|s| (READ_PATH, s.to_owned())));
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
        credentials: Box::new(Keyed(secrets)),
        clock: e.clock.clone(),
    })
    .unwrap();
    Rig { e, b, url, seen }
}

pub fn submit(r: &Rig) -> Result<(RequestKey, String), BrokerError> {
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

pub fn remote_ref(r: &Rig) -> String {
    git(&r.e.remote, &["rev-parse", "refs/heads/agent/x"])
        .trim()
        .to_owned()
}

/// Answers every request `302` to `to` and records whether it carried any `Authorization`.
pub fn redirector(to: String) -> (u16, Arc<Mutex<Vec<bool>>>) {
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
