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

#[cfg(unix)]
mod reply_tests {
    use super::super::judge_reply;

    #[test]
    fn a_result_for_the_probe_id_is_up() {
        let r = judge_reply(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
        assert!(r.unwrap().is_ok());
    }

    #[test]
    fn a_communication_denied_error_is_a_failure() {
        let r = judge_reply(
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32011,"message":"CommunicationDenied"}}"#,
        );
        let msg = format!("{:#}", r.unwrap().unwrap_err());
        assert!(
            msg.contains("-32011") && msg.contains("CommunicationDenied"),
            "{msg}"
        );
    }

    #[test]
    fn notifications_other_ids_and_garbage_are_skipped() {
        assert!(judge_reply(br#"{"jsonrpc":"2.0","method":"event","params":{}}"#).is_none());
        assert!(judge_reply(br#"{"jsonrpc":"2.0","id":2,"result":{}}"#).is_none());
        assert!(judge_reply(b"not json").is_none());
        assert!(judge_reply(br#"{"jsonrpc":"2.0","id":1}"#).is_none());
    }
}

/// Socket-level regressions for the two review findings. Same sandbox gate
/// as the test above.
#[cfg(unix)]
#[test]
fn socket_probe_rejects_errors_skips_noise_and_holds_its_deadline() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping socket regression test: set MUR_TEST_SOCKETS=1");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Serve one connection per script, each writing `script` after the request.
    let serve = |name: &str, script: Vec<(u64, &'static [u8])>| {
        let sock = tmp.path().join(name);
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            for (delay_ms, bytes) in script {
                std::thread::sleep(Duration::from_millis(delay_ms));
                if (&stream).write_all(bytes).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(3)); // hold the peer open
        });
        sock
    };

    // 1. A refusal is a failure.
    let denied = serve(
        "denied.sock",
        vec![(
            0,
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32011,"message":"CommunicationDenied"}}
"#,
        )],
    );
    let err = probe_socket(denied.to_str().unwrap(), Duration::from_secs(2)).unwrap_err();
    assert!(format!("{err:#}").contains("-32011"), "{err:#}");

    // 2. A notification before the reply does not end the probe early.
    let noisy = serve(
        "noisy.sock",
        vec![
            (
                0,
                b"{\"jsonrpc\":\"2.0\",\"method\":\"event\",\"params\":{}}\n",
            ),
            (50, b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n"),
        ],
    );
    probe_socket(noisy.to_str().unwrap(), Duration::from_secs(2)).unwrap();

    // 3. A peer trickling bytes without a newline cannot outlast the deadline.
    let trickle = serve(
        "trickle.sock",
        (0..20).map(|_| (200u64, b"x" as &'static [u8])).collect(),
    );
    let started = Instant::now();
    let err = probe_socket(trickle.to_str().unwrap(), Duration::from_secs(1)).unwrap_err();
    let took = started.elapsed();
    assert!(took < Duration::from_millis(1600), "took {took:?}: {err:#}");
}
