//! Tests for `preflight.rs` (#1762): the pre-start member checks.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow};
use mur_common::limits::Stuck;

use super::*;

/// A probe whose `down` members never answer.
struct Probe {
    down: BTreeSet<&'static str>,
}

impl MemberProbe for Probe {
    fn round_trip(&self, _: &Path, member: &str) -> Result<()> {
        if self.down.contains(member) {
            Err(anyhow!("connect agent.sock: Connection refused"))
        } else {
            Ok(())
        }
    }
}

fn up() -> Probe {
    Probe {
        down: BTreeSet::new(),
    }
}

fn agent(home: &Path, name: &str, profile_tail: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        format!("name: {name}\n{profile_tail}"),
    )
    .unwrap();
}

fn stuck_10m() -> Stuck {
    Stuck::After(Duration::from_secs(600))
}

#[test]
fn required_timeout_is_the_stuck_window_rounded_up() {
    assert_eq!(required_hitl_timeout(Stuck::Off), None);
    assert_eq!(required_hitl_timeout(stuck_10m()), Some(600));
    assert_eq!(
        required_hitl_timeout(Stuck::After(Duration::from_millis(600_500))),
        Some(601)
    );
}

#[test]
fn passes_when_members_answer_and_timeouts_cover_the_window() {
    let tmp = tempfile::tempdir().unwrap();
    agent(tmp.path(), "a", "hitl:\n  timeout_secs: 600\n");
    agent(tmp.path(), "b", "hitl:\n  timeout_secs: 3600\n");
    preflight_members(tmp.path(), &["a", "b"], stuck_10m(), &up()).unwrap();
}

#[test]
fn a_default_profile_is_too_short_for_the_default_window() {
    // No hitl block = the built-in 300 s, shorter than the 10 m stuck window:
    // the exact failure #1762 reports.
    let tmp = tempfile::tempdir().unwrap();
    agent(tmp.path(), "a", "");
    agent(tmp.path(), "b", "hitl:\n  timeout_secs: 600\n");
    let err = preflight_members(tmp.path(), &["a", "b"], stuck_10m(), &up())
        .unwrap_err()
        .to_string();
    assert!(err.contains("'a' has hitl.timeout_secs 300"), "{err}");
    assert!(err.contains("at least 600"), "{err}");
    assert!(err.contains("mur agent restart a"), "{err}");
    assert!(!err.contains("'b'"), "b is fine: {err}");
}

#[test]
fn stuck_off_has_no_window_to_outlast() {
    let tmp = tempfile::tempdir().unwrap();
    agent(tmp.path(), "a", "");
    agent(tmp.path(), "b", "");
    preflight_members(tmp.path(), &["a", "b"], Stuck::Off, &up()).unwrap();
}

#[test]
fn a_member_that_does_not_answer_is_named_with_the_fix() {
    let tmp = tempfile::tempdir().unwrap();
    agent(tmp.path(), "a", "hitl:\n  timeout_secs: 600\n");
    agent(tmp.path(), "b", "hitl:\n  timeout_secs: 600\n");
    let probe = Probe {
        down: BTreeSet::from(["b"]),
    };
    let err = preflight_members(tmp.path(), &["a", "b"], stuck_10m(), &probe)
        .unwrap_err()
        .to_string();
    assert!(err.contains("'b' does not answer"), "{err}");
    assert!(err.contains("Connection refused"), "{err}");
    assert!(err.contains("mur agent restart b"), "{err}");
    assert!(!err.contains("'a'"), "{err}");
}

#[test]
fn every_problem_is_reported_in_one_run() {
    let tmp = tempfile::tempdir().unwrap();
    agent(tmp.path(), "a", "");
    agent(tmp.path(), "b", "hitl:\n  timeout_secs: 600\n");
    let probe = Probe {
        down: BTreeSet::from(["a", "b"]),
    };
    let err = preflight_members(tmp.path(), &["a", "b"], stuck_10m(), &probe)
        .unwrap_err()
        .to_string();
    assert!(err.contains("'a' does not answer"), "{err}");
    assert!(err.contains("'b' does not answer"), "{err}");
    assert!(err.contains("'a' has hitl.timeout_secs 300"), "{err}");
}

#[test]
fn a_timeout_equal_to_the_window_passes() {
    let tmp = tempfile::tempdir().unwrap();
    agent(tmp.path(), "a", "hitl:\n  timeout_secs: 600\n");
    preflight_members(tmp.path(), &["a"], stuck_10m(), &up()).unwrap();
}

#[test]
fn a_non_numeric_timeout_is_reported_not_skipped() {
    // The check cannot judge it; saying nothing would pass a profile it never
    // read.
    let tmp = tempfile::tempdir().unwrap();
    agent(tmp.path(), "a", "hitl:\n  timeout_secs: soon\n");
    let err = preflight_members(tmp.path(), &["a"], stuck_10m(), &up())
        .unwrap_err()
        .to_string();
    assert!(err.contains("'a': cannot read hitl.timeout_secs"), "{err}");
    assert!(err.contains("not a number"), "{err}");
}

#[cfg(unix)]
#[test]
fn socket_probe_sees_a_reply_and_a_dead_socket() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    // AF_UNIX connects are not permitted in every sandbox; same gate as
    // `a2a_dial::timeout_tests`.
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping socket_probe_sees_a_reply_and_a_dead_socket: set MUR_TEST_SOCKETS=1");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("s.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        (&stream)
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
            .unwrap();
    });
    probe_socket(sock.to_str().unwrap(), Duration::from_secs(2)).unwrap();
    server.join().unwrap();
    // The listener is gone; the socket file remains: the stale-socket case.
    let err = probe_socket(sock.to_str().unwrap(), Duration::from_secs(2)).unwrap_err();
    assert!(format!("{err:#}").contains("connect"), "{err:#}");
}
