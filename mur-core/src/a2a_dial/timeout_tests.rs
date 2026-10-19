//! Regression coverage for the dogfood hang: `mur agent send` blocked
//! indefinitely against a peer that accepted the connection but never
//! wrote a response line (simulating a stalled/wedged runtime, or one
//! silently waiting past the client's patience). `dial_socket` must now
//! bound the wait and return an actionable error instead of hanging.
//!
//! Gated behind `MUR_TEST_SOCKETS=1`: some sandboxed CI/dev environments
//! deny `AF_UNIX` `connect(2)` outright (observed as `EPERM`, "Operation
//! not permitted", even against a freshly bound `/tmp` socket with a
//! same-process listener already accepting) regardless of socket path
//! length or `TMPDIR` location. The test is fully functional on CI and
//! normal dev machines; set the env var there (or anywhere unix-domain
//! sockets are actually permitted) to run it.
use super::*;
use std::io::Read;
use std::os::unix::net::UnixListener;

/// Serial guard: `MUR_A2A_IO_TIMEOUT_SECS` is process-global env, and
/// `cargo test` runs tests in this file concurrently by default.

#[test]
fn dial_socket_times_out_instead_of_hanging_forever() {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!(
            "skipping dial_socket_times_out_instead_of_hanging_forever: \
                 set MUR_TEST_SOCKETS=1 on a machine that permits AF_UNIX \
                 connects (see module docs for why this is gated)"
        );
        return;
    }
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::TempDir::new().unwrap();
    let sock_path = tmp.path().join("agent.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();

    // Fake peer: accepts the connection, reads the request so the
    // client's write doesn't itself block, then goes silent forever
    // (drops the byte stream only when the test ends and the listener
    // thread's stream is dropped).
    let server = std::thread::spawn(move || {
        if let Ok((mut conn, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            // Drain whatever the client sent; ignore the result — we
            // never reply, which is the whole point of this test.
            let _ = conn.read(&mut buf);
            // Hold the connection open (don't drop `conn`) well past
            // the client's configured timeout, so a passing test proves
            // the client-side timeout fired rather than an EOF race.
            std::thread::sleep(Duration::from_secs(5));
        }
    });

    let agent_dir = tmp.path().join("agents").join("stalled");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("running.lock"),
        serde_json::json!({
            "schema": 1,
            "uuid": "u",
            "name": "stalled",
            "pid": 1,
            "ppid": 1,
            "started_at": "t",
            "binary_version": "test",
            "transports": { "stdio": true, "unix_socket": sock_path.to_str().unwrap() },
            "card_digest": "d",
            "capabilities": [],
        })
        .to_string(),
    )
    .unwrap();

    // (test-only env mutation): serialized by the EnvGuard above so
    // no other test in this process observes a torn value.
    envg.set_var("MUR_A2A_IO_TIMEOUT_SECS", "1");
    let start = std::time::Instant::now();
    let result = dial_method(
        tmp.path(),
        "stalled",
        "message/send",
        serde_json::json!({}),
        DialMode::RequireRunning,
    );
    let elapsed = start.elapsed();
    envg.unset_var("MUR_A2A_IO_TIMEOUT_SECS");

    let err = result.unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("did not respond") && msg.contains("stalled"),
        "expected an actionable timeout error, got: {msg}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "dial_socket should time out near the configured 1s bound, took {elapsed:?} \
             (a regression here means the hang is back)"
    );

    let _ = server.join();
}

fn lock_json(name: &str, sock: &std::path::Path, proto: u32) -> serde_json::Value {
    serde_json::json!({
        "schema": 1,
        "uuid": "u",
        "name": name,
        "pid": 1,
        "ppid": 1,
        "started_at": "t",
        "binary_version": "test",
        "proto_version": proto,
        "transports": { "stdio": true, "unix_socket": sock.to_str().unwrap() },
        "card_digest": "d",
        "capabilities": [],
    })
}

fn write_lock(home: &std::path::Path, name: &str, sock: &std::path::Path, proto: u32) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("running.lock"),
        lock_json(name, sock, proto).to_string(),
    )
    .unwrap();
}

fn sockets_allowed(test: &str) -> bool {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!(
            "skipping {test}: set MUR_TEST_SOCKETS=1 on a machine that permits AF_UNIX connects"
        );
        return false;
    }
    true
}

/// The timeout is chosen by the PEER's proto: a legacy lock keeps 600 s,
/// a beating one gets 90 s, and the env override beats both.
#[test]
fn idle_timeout_follows_the_peers_proto() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.unset_var("MUR_A2A_IO_TIMEOUT_SECS");
    let p = std::path::Path::new("/nonexistent.sock");
    let legacy: LockFile = serde_json::from_value(lock_json("a", p, 1)).unwrap();
    let beating: LockFile = serde_json::from_value(lock_json("a", p, 2)).unwrap();
    assert_eq!(
        dial_io_timeout_for(legacy.proto_version),
        LEGACY_DIAL_IO_TIMEOUT
    );
    assert_eq!(
        dial_io_timeout_for(beating.proto_version),
        HEARTBEAT_DIAL_IO_TIMEOUT
    );
    envg.set_var("MUR_A2A_IO_TIMEOUT_SECS", "7");
    assert_eq!(
        dial_io_timeout_for(beating.proto_version),
        Duration::from_secs(7)
    );
    envg.unset_var("MUR_A2A_IO_TIMEOUT_SECS");
}

/// A proto-2 peer that beats every 300 ms while it "thinks" for 2 s is
/// alive: a 1 s idle timeout never fires, and the response arrives.
#[test]
fn heartbeats_reset_the_idle_timeout() {
    use std::io::Write;
    if !sockets_allowed("heartbeats_reset_the_idle_timeout") {
        return;
    }
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::TempDir::new().unwrap();
    let sock_path = tmp.path().join("agent.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let server = std::thread::spawn(move || {
        if let Ok((mut conn, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            for _ in 0..7 {
                std::thread::sleep(Duration::from_millis(300));
                let _ = conn.write_all(
                        b"{\"jsonrpc\":\"2.0\",\"method\":\"turn/heartbeat\",\"params\":{\"task_id\":\"t\",\"at\":\"2026-09-12T00:00:00Z\"}}\n",
                    );
            }
            let _ = conn.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n");
        }
    });
    write_lock(tmp.path(), "beating", &sock_path, 2);
    envg.set_var("MUR_A2A_IO_TIMEOUT_SECS", "1");
    let result = dial_method(
        tmp.path(),
        "beating",
        "message/send",
        serde_json::json!({}),
        DialMode::RequireRunning,
    );
    envg.unset_var("MUR_A2A_IO_TIMEOUT_SECS");
    let v = result.expect("heartbeats must keep the dial alive");
    assert_eq!(v["ok"], true);
    let _ = server.join();
}

/// A proto-2 peer that goes silent is reported as STOPPED RESPONDING,
/// naming the last frame — not as "did not respond", which is what a
/// legacy peer with no heartbeats gets.
#[test]
fn a_silent_beating_peer_stopped_responding() {
    use std::io::Write;
    if !sockets_allowed("a_silent_beating_peer_stopped_responding") {
        return;
    }
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::TempDir::new().unwrap();
    let sock_path = tmp.path().join("agent.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let _server = std::thread::spawn(move || {
        if let Ok((mut conn, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let _ = conn.write_all(
                    b"{\"jsonrpc\":\"2.0\",\"method\":\"turn/heartbeat\",\"params\":{\"task_id\":\"t\",\"at\":\"2026-09-12T00:00:00Z\"}}\n",
                );
            std::thread::sleep(Duration::from_secs(4));
        }
    });
    write_lock(tmp.path(), "silent", &sock_path, 2);
    envg.set_var("MUR_A2A_IO_TIMEOUT_SECS", "1");
    let err = dial_method(
        tmp.path(),
        "silent",
        "message/send",
        serde_json::json!({}),
        DialMode::RequireRunning,
    )
    .unwrap_err();
    envg.unset_var("MUR_A2A_IO_TIMEOUT_SECS");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("stopped responding")
            && msg.contains("turn/heartbeat")
            && msg.contains("2026-09-12T00:00:00Z"),
        "{msg}"
    );
}

/// #1758: a peer that only beats is alive but idle. The poll sees `idle`
/// grow past the heartbeats and the task id from the first frame, and its
/// error ends the dial long before the response would arrive.
#[test]
fn poll_sees_heartbeats_as_idle_and_can_end_the_dial() {
    use std::io::Write;
    if !sockets_allowed("poll_sees_heartbeats_as_idle_and_can_end_the_dial") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let sock_path = tmp.path().join("agent.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let server = std::thread::spawn(move || {
        if let Ok((mut conn, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            for _ in 0..30 {
                if conn
                    .write_all(
                        b"{\"jsonrpc\":\"2.0\",\"method\":\"turn/heartbeat\",\"params\":{\"task_id\":\"task-7\",\"at\":\"2026-09-12T00:00:00Z\"}}\n",
                    )
                    .is_err()
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    });
    write_lock(tmp.path(), "beating", &sock_path, 2);
    let mut seen: Option<(Duration, Option<String>)> = None;
    let mut check = |s: &PollState<'_>| {
        if s.idle >= Duration::from_millis(400) {
            seen = Some((s.idle, s.task_id.map(str::to_string)));
            return Some(anyhow!("preempt"));
        }
        None
    };
    let started = Instant::now();
    let err = dial_message_streaming_polled(
        tmp.path(),
        "beating",
        serde_json::json!({}),
        |_, _, _| {},
        |_| {},
        |_| {},
        Some(DialPoll {
            every: Duration::from_millis(50),
            check: &mut check,
        }),
    )
    .expect_err("the poll must end the dial");
    assert_eq!(err.to_string(), "preempt");
    assert!(started.elapsed() < Duration::from_secs(2), "preempted late");
    let (_, task) = seen.expect("poll tripped");
    assert_eq!(task.as_deref(), Some("task-7"));
    let _ = server.join();
}

/// #1907 follow-up: a denial stamped for ANOTHER turn — a broadcast from a
/// runtime that predates routing, arriving before this connection's own
/// first frame — must neither print here nor be mistaken for this turn's id.
/// Unstamped denials (no turn to attribute) still print, as before.
#[test]
fn another_turns_denial_is_neither_shown_nor_adopted_as_this_turns_id() {
    use std::io::Write;
    if !sockets_allowed("another_turns_denial_is_neither_shown_nor_adopted_as_this_turns_id") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let sock_path = tmp.path().join("agent.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let denied = |task: &str| {
        let task = if task.is_empty() {
            String::new()
        } else {
            format!(",\"task_id\":\"{task}\"")
        };
        format!(
            "{{\"jsonrpc\":\"2.0\",\"method\":\"egress/denied\",\"params\":{{\"agent\":\"mur\",\"server\":\"browser\",\"host\":\"h{}\",\"port\":443,\"mode\":\"restricted\",\"reason\":\"not_allowed\",\"at\":\"t\"{task}}}}}\n",
            task.len()
        )
    };
    let frames = [
        denied("task-other"),
        denied(""),
        "{\"jsonrpc\":\"2.0\",\"method\":\"turn/heartbeat\",\"params\":{\"task_id\":\"task-mine\",\"at\":\"t\"}}\n".to_string(),
        denied("task-mine"),
        denied("task-other"),
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n".to_string(),
    ];
    let server = std::thread::spawn(move || {
        if let Ok((mut conn, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            for f in frames {
                let _ = conn.write_all(f.as_bytes());
            }
        }
    });
    write_lock(tmp.path(), "two-sessions", &sock_path, 2);
    let mut shown: Vec<Option<String>> = vec![];
    dial_message_streaming(
        tmp.path(),
        "two-sessions",
        serde_json::json!({}),
        |_, _, _| {},
        |_| {},
        |e| {
            if let StepEvent::EgressDenied(ev) = e {
                shown.push(ev.task_id);
            }
        },
    )
    .expect("dial");
    let _ = server.join();
    // Had the first `task-other` been adopted as this turn's id, it would
    // show here and `task-mine` would not.
    assert_eq!(shown, vec![None, Some("task-mine".to_string())]);
}
