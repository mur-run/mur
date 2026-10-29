//! ssh-agent proxy, option A, Verification 4 (spec: "Destination-constrained
//! keys over an unbound upstream connection"). The proxy verifies
//! `session-bind` locally and never forwards it, so the upstream only ever
//! sees an *unbound* connection. This records what a real OpenSSH agent does
//! with a key added via `ssh-add -h <dest>` on such a connection.
//!
//! The result is recorded, not asserted: either outcome is a valid finding
//! (a refusal becomes a user-facing v1 limitation). Only the control — an
//! unconstrained key signing on the same connection — is asserted, so a
//! broken harness cannot masquerade as "refused".
//!
//! No sandbox is involved, so this runs anywhere OpenSSH ≥ 8.9 is on PATH.
//! A missing `ssh-agent`/`ssh-add`/`ssh-keygen` skips locally and fails
//! under CI.
#![cfg(unix)]

use base64::Engine;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// ssh-agent protocol (draft-miller-ssh-agent).
const SSH_AGENT_FAILURE: u8 = 5;
const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;
const SSH_AGENTC_SIGN_REQUEST: u8 = 13;
const SSH_AGENT_SIGN_RESPONSE: u8 = 14;

/// Destination host named in the constraint; never contacted.
const DEST_HOST: &str = "o7-dest.invalid";
const CONSTRAINED_COMMENT: &str = "o7-constrained";
const CONTROL_COMMENT: &str = "o7-control";
/// Payload to sign; content is irrelevant to the agent's permit decision.
const SIGN_PAYLOAD: &[u8] = b"o7 verification 4";

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const AGENT_START_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);
const CI_ENV_VAR: &str = "CI";

fn in_ci() -> bool {
    std::env::var_os(CI_ENV_VAR).is_some_and(|v| !v.is_empty() && v != "false" && v != "0")
}

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("-h")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Kills the agent even when an assertion unwinds.
struct Agent(Child);
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(cmd: &mut Command, what: &str) {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{what}: spawn: {e}"));
    assert!(
        out.status.success(),
        "{what} failed: {:?}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn keygen(path: &Path, comment: &str) {
    run(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", comment, "-f"])
            .arg(path),
        "ssh-keygen",
    );
}

/// Wire blob of a public key, decoded from its `.pub` file.
fn pub_blob(key: &Path) -> Vec<u8> {
    let text = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    let b64 = text.split_whitespace().nth(1).expect("pub key field");
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap()
}

fn put_string(v: &mut Vec<u8>, s: &[u8]) {
    v.extend_from_slice(&(s.len() as u32).to_be_bytes());
    v.extend_from_slice(s);
}

fn request(s: &mut UnixStream, body: &[u8]) -> Vec<u8> {
    let mut msg = (body.len() as u32).to_be_bytes().to_vec();
    msg.extend_from_slice(body);
    s.write_all(&msg).unwrap();
    let mut len = [0u8; 4];
    s.read_exact(&mut len).unwrap();
    let mut reply = vec![0u8; u32::from_be_bytes(len) as usize];
    s.read_exact(&mut reply).unwrap();
    reply
}

/// Blobs in an IDENTITIES_ANSWER.
fn listed_blobs(reply: &[u8]) -> Vec<Vec<u8>> {
    assert_eq!(reply.first(), Some(&SSH_AGENT_IDENTITIES_ANSWER));
    let mut p = &reply[1..];
    let take = |p: &mut &[u8]| {
        let n = u32::from_be_bytes(p[..4].try_into().unwrap()) as usize;
        let s = p[4..4 + n].to_vec();
        *p = &p[4 + n..];
        s
    };
    let count = u32::from_be_bytes(p[..4].try_into().unwrap());
    p = &p[4..];
    (0..count)
        .map(|_| {
            let blob = take(&mut p);
            let _comment = take(&mut p);
            blob
        })
        .collect()
}

fn sign(s: &mut UnixStream, blob: &[u8]) -> String {
    let mut body = vec![SSH_AGENTC_SIGN_REQUEST];
    put_string(&mut body, blob);
    put_string(&mut body, SIGN_PAYLOAD);
    body.extend_from_slice(&0u32.to_be_bytes());
    match request(s, &body).first() {
        Some(&SSH_AGENT_SIGN_RESPONSE) => "signed".into(),
        Some(&SSH_AGENT_FAILURE) => "refused".into(),
        other => format!("unexpected:{other:?}"),
    }
}

fn version() -> String {
    Command::new("ssh")
        .arg("-V")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_owned())
        .unwrap_or_else(|_| "unknown".into())
}

#[test]
fn verification_4_destination_constrained_key_on_unbound_connection() {
    let bins = ["ssh-agent", "ssh-add", "ssh-keygen"];
    if let Some(missing) = bins.iter().find(|b| !have(b)) {
        assert!(!in_ci(), "{missing} not on PATH under CI");
        eprintln!("SKIP verification_4: {missing} not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = std::fs::canonicalize(dir.path()).unwrap();
    let sock: PathBuf = d.join("agent.sock");

    let _agent = Agent(
        Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&sock)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("ssh-agent"),
    );
    let start = Instant::now();
    while !sock.exists() {
        assert!(
            start.elapsed() < AGENT_START_TIMEOUT,
            "ssh-agent never bound"
        );
        std::thread::sleep(POLL);
    }

    let host_key = d.join("host_key");
    let constrained = d.join("constrained");
    let control = d.join("control");
    keygen(&host_key, "o7-host");
    keygen(&constrained, CONSTRAINED_COMMENT);
    keygen(&control, CONTROL_COMMENT);

    // `ssh-add -h` resolves the destination's host key from known_hosts.
    let known_hosts = d.join("known_hosts");
    let host_pub = std::fs::read_to_string(host_key.with_extension("pub")).unwrap();
    let mut fields = host_pub.split_whitespace();
    let (kt, kb) = (fields.next().unwrap(), fields.next().unwrap());
    std::fs::write(&known_hosts, format!("{DEST_HOST} {kt} {kb}\n")).unwrap();

    run(
        Command::new("ssh-add")
            .env("SSH_AUTH_SOCK", &sock)
            .arg("-H")
            .arg(&known_hosts)
            .args(["-h", DEST_HOST])
            .arg(&constrained),
        "ssh-add -h",
    );
    run(
        Command::new("ssh-add")
            .env("SSH_AUTH_SOCK", &sock)
            .arg(&control),
        "ssh-add (control)",
    );

    // One unbound connection, as the proxy holds it: no session-bind sent.
    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
    s.set_write_timeout(Some(IO_TIMEOUT)).unwrap();

    let listed = listed_blobs(&request(&mut s, &[SSH_AGENTC_REQUEST_IDENTITIES]));
    let (c_blob, k_blob) = (pub_blob(&constrained), pub_blob(&control));
    let constrained_listed = listed.contains(&c_blob);
    let control_sign = sign(&mut s, &k_blob);
    let constrained_sign = sign(&mut s, &c_blob);

    eprintln!(
        "O7 verification 4 [{}]: control_listed={} control_sign={} constrained_listed={} constrained_sign={}",
        version(),
        listed.contains(&k_blob),
        control_sign,
        constrained_listed,
        constrained_sign
    );
    // Harness check only: the unconstrained key must sign on this
    // connection, or "refused" below would mean nothing.
    assert_eq!(
        control_sign, "signed",
        "control key did not sign on an unbound connection; measurement invalid"
    );
    assert!(
        constrained_sign == "signed" || constrained_sign == "refused",
        "constrained sign gave {constrained_sign}"
    );
}
