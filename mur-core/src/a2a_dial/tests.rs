use super::*;
use tempfile::tempdir;

#[test]
fn require_running_fails_without_lock() {
    let home = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/nobody")).unwrap();
    let err = dial_method(
        home.path(),
        "nobody",
        "agent/card",
        Value::Null,
        DialMode::RequireRunning,
    )
    .unwrap_err();
    assert!(err.to_string().contains("not running"));
}

#[test]
fn auto_mode_falls_through_to_ephemeral_when_no_lock() {
    // Without a runtime binary on PATH the ephemeral spawn fails
    // with a recognizable error — that's what we assert, since this
    // is a pure unit test.
    let home = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/nobody")).unwrap();
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_AGENT_RUNTIME_BIN", "/does/not/exist");
    let err = dial_method(
        home.path(),
        "nobody",
        "agent/card",
        Value::Null,
        DialMode::Auto,
    )
    .unwrap_err();
    envg.unset_var("MUR_AGENT_RUNTIME_BIN");
    let msg = err.to_string();
    assert!(
        msg.contains("runtime binary not found")
            || msg.contains("spawn")
            || msg.contains("attestation"),
        "unexpected error: {msg}"
    );
}

#[test]
fn dial_gates_channel_delegate_on_stale_proto() {
    let tmp = tempfile::TempDir::new().unwrap();
    let adir = tmp.path().join("agents").join("rustsmith");
    std::fs::create_dir_all(&adir).unwrap();
    // Old lock: proto_version absent → 0 < channel/delegate's min (1).
    std::fs::write(
        adir.join("running.lock"),
        r#"{"schema":1,"uuid":"u",
          "name":"rustsmith","pid":1,"ppid":1,"started_at":"t",
          "binary_version":"old","transports":{"stdio":true,
          "unix_socket":"/nonexistent.sock"},"card_digest":"d","capabilities":[]}"#,
    )
    .unwrap();

    let err = dial_method(
        tmp.path(),
        "rustsmith",
        "channel/delegate",
        serde_json::json!({}),
        DialMode::RequireRunning,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("stale runtime"), "got: {msg}");
    assert!(msg.contains("mur agent restart rustsmith"), "got: {msg}");
    // It must NOT have tried to connect to the (nonexistent) socket.
    assert!(
        !msg.contains("connect"),
        "gate should fire before dialing: {msg}"
    );
}

#[test]
fn dial_does_not_gate_ungated_method() {
    // message/send (min 0) on the same stale lock must NOT be proto-gated
    // (it will fail later for a different reason — socket — which is fine).
    let tmp = tempfile::TempDir::new().unwrap();
    let adir = tmp.path().join("agents").join("a");
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::write(
        adir.join("running.lock"),
        r#"{"schema":1,"uuid":"u","name":"a",
          "pid":1,"ppid":1,"started_at":"t","binary_version":"old",
          "transports":{"stdio":true,"unix_socket":"/nonexistent.sock"},
          "card_digest":"d","capabilities":[]}"#,
    )
    .unwrap();
    let err = dial_method(
        tmp.path(),
        "a",
        "message/send",
        serde_json::json!({}),
        DialMode::RequireRunning,
    )
    .unwrap_err();
    assert!(
        !err.to_string().contains("stale runtime"),
        "must not gate message/send"
    );
}
