use super::*;
/// A `/bin/sh -c body` "server", spawned with piped stdio. `/bin/sh`
/// itself rather than a script file: a sealed test shell may refuse to exec
/// a freshly written file, and the handshake is what is under test.
fn fake(_dir: &Path, body: &str) -> Child {
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(body);
    server_command(&mut c).spawn().unwrap()
}

/// Shell that prints `msg` as one LSP frame.
fn emit(msg: &Value) -> String {
    let body = msg.to_string();
    format!(
        "printf 'Content-Length: {}\\r\\n\\r\\n%s' '{}'",
        body.len(),
        body.replace('\'', r"'\''")
    )
}

#[test]
fn a_reply_after_a_notification_is_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let log =
        json!({"jsonrpc":"2.0","method":"window/logMessage","params":{"type":3,"message":"hi"}});
    let reply = json!({"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}});
    let body = format!("{}\n{}\nsleep 5", emit(&log), emit(&reply));
    let r = handshake(fake(tmp.path(), &body), tmp.path(), Duration::from_secs(10));
    assert_eq!(r, Report::Ok);
}

#[test]
fn an_error_reply_is_a_failure_naming_the_error() {
    let tmp = tempfile::tempdir().unwrap();
    let reply = json!({"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"boom"}});
    let r = handshake(
        fake(tmp.path(), &format!("{}\nsleep 5", emit(&reply))),
        tmp.path(),
        Duration::from_secs(10),
    );
    let Report::Failed { detail } = r else {
        panic!("{r:?}")
    };
    assert!(
        detail.contains("refused") && detail.contains("boom"),
        "{detail}"
    );
}

/// The case setup exists to catch: the server dies on a sandbox EPERM and
/// says so on stderr. The user must see that line, not just "it exited".
#[test]
fn an_early_exit_quotes_the_stderr_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "echo 'Error: EPERM: operation not permitted, mkdir /x' >&2\nexit 1";
    let r = handshake(fake(tmp.path(), body), tmp.path(), Duration::from_secs(10));
    let Report::Failed { detail } = r else {
        panic!("{r:?}")
    };
    assert!(detail.contains("before"), "{detail}");
    assert!(
        detail.contains("EPERM: operation not permitted"),
        "{detail}"
    );
}

#[test]
fn silence_times_out_and_the_server_is_stopped() {
    let tmp = tempfile::tempdir().unwrap();
    let start = std::time::Instant::now();
    let r = handshake(
        fake(tmp.path(), "exec sleep 30"),
        tmp.path(),
        Duration::from_millis(300),
    );
    let Report::Failed { detail } = r else {
        panic!("{r:?}")
    };
    assert!(detail.contains("no answer"), "{detail}");
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the hung server was not killed"
    );
}

#[test]
fn frames_round_trip_and_oversized_ones_are_refused() {
    let msg = initialize(Path::new("/w/repo"));
    let mut r = std::io::Cursor::new(frame(&msg));
    assert_eq!(read_frame(&mut r).unwrap(), msg);
    assert_eq!(msg["params"]["rootUri"], "file:///w/repo/");
    assert_eq!(msg["params"]["workspaceFolders"][0]["name"], "repo");

    let huge = format!("Content-Length: {}\r\n\r\n", MAX_FRAME + 1);
    assert!(read_frame(&mut std::io::Cursor::new(huge)).is_err());
    assert!(read_frame(&mut std::io::Cursor::new("X: 1\r\n\r\n{}")).is_err());
}

#[test]
fn the_report_wire_shape_is_tagged() {
    let r = Report::Failed { detail: "x".into() };
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r#"{"status":"failed","detail":"x"}"#
    );
    assert_eq!(
        serde_json::to_string(&Report::Ok).unwrap(),
        r#"{"status":"ok"}"#
    );
}

/// Review of #1988: a server's descendant that inherited stderr kept the pipe
/// open after the direct child was killed, so the stderr join blocked until
/// the descendant exited. The whole group must go, and the drain is bounded.
#[test]
fn a_descendant_holding_stderr_does_not_outlive_the_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("grandchild.pid");
    // The grandchild keeps the inherited stderr and outlives the shell.
    let body = format!(
        "sleep 30 & echo $! > '{}'\nexec sleep 30",
        pid_file.display()
    );
    let start = std::time::Instant::now();
    let r = handshake(
        fake(tmp.path(), &body),
        tmp.path(),
        Duration::from_millis(300),
    );
    let elapsed = start.elapsed();
    let Report::Failed { detail } = r else {
        panic!("{r:?}")
    };
    assert!(detail.contains("no answer"), "{detail}");
    assert!(
        elapsed < Duration::from_secs(10),
        "the probe waited {elapsed:?} on a descendant's stderr"
    );
    let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only probes for existence.
    let alive = (0..50).any(|_| {
        let gone = unsafe { libc::kill(pid, 0) } != 0;
        if !gone {
            std::thread::sleep(Duration::from_millis(100));
        }
        !gone
    }) && unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "the server's descendant {pid} was left running");
}

/// The drain bound holds even when a descendant escaped the group (setsid):
/// nothing can kill it, so the probe must stop waiting for its stderr.
#[test]
fn stderr_collection_is_bounded() {
    let (r, _w) = std::io::pipe().unwrap();
    let start = std::time::Instant::now();
    assert_eq!(bounded_tail(r, Duration::from_millis(200)), "");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn an_already_sealed_shell_is_unverified() {
    let r = seal_verdict(true, None).expect("must not proceed");
    assert!(
        matches!(&r, Report::Unverified { detail } if detail.contains("already")),
        "{r:?}"
    );
}

#[test]
fn a_non_enforcing_seal_is_unverified() {
    let r = seal_verdict(false, Some(Ok((false, "unsupported".into())))).expect("must not proceed");
    assert!(
        matches!(&r, Report::Unverified { detail } if detail.contains("not enforcing") && detail.contains("unsupported")),
        "{r:?}"
    );
}

#[test]
fn an_enforcing_seal_proceeds_and_a_refused_one_fails() {
    assert_eq!(seal_verdict(false, Some(Ok((true, "macos".into())))), None);
    let r = seal_verdict(false, Some(Err(anyhow::anyhow!("nope")))).unwrap();
    assert!(
        matches!(&r, Report::Failed { detail } if detail.contains("nope")),
        "{r:?}"
    );
}
