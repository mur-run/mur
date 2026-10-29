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

// ---------------------------------------------------------------------------
// canonicalize_agent_name (#2208)
// ---------------------------------------------------------------------------

fn make_agent(home: &std::path::Path, dir: &str) {
    let d = home.join("agents").join(dir);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("profile.yaml"), format!("name: {dir}\n")).unwrap();
}

/// The regression: on a case-insensitive filesystem `agents/Mur/profile.yaml`
/// "exists" when the directory is `mur`, and the old exact-match probe handed
/// `Mur` back. The listing is the source of truth on every filesystem.
#[test]
fn canonicalize_returns_on_disk_spelling() {
    let home = tempdir().unwrap();
    make_agent(home.path(), "mur");
    assert_eq!(canonicalize_agent_name(home.path(), "Mur"), "mur");
    assert_eq!(canonicalize_agent_name(home.path(), "MUR"), "mur");
    assert_eq!(canonicalize_agent_name(home.path(), "mur"), "mur");
}

#[test]
fn canonicalize_leaves_unknown_names_unchanged() {
    let home = tempdir().unwrap();
    assert_eq!(canonicalize_agent_name(home.path(), "Ghost"), "Ghost");
    make_agent(home.path(), "mur");
    assert_eq!(canonicalize_agent_name(home.path(), "Ghost"), "Ghost");
}

/// A directory without `profile.yaml` is not an agent.
#[test]
fn canonicalize_ignores_dirs_without_profile() {
    let home = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents").join("mur")).unwrap();
    assert_eq!(canonicalize_agent_name(home.path(), "Mur"), "Mur");
}

/// Two case variants can only coexist on a case-sensitive filesystem. Exact
/// spelling wins; with none exact it is ambiguous and the input comes back.
#[test]
fn canonicalize_case_variants_exact_wins_else_ambiguous() {
    let home = tempdir().unwrap();
    make_agent(home.path(), "mur");
    let variant = home.path().join("agents").join("Mur");
    if variant.exists() {
        return; // case-insensitive filesystem: the scenario cannot exist
    }
    make_agent(home.path(), "Mur");
    assert_eq!(canonicalize_agent_name(home.path(), "mur"), "mur");
    assert_eq!(canonicalize_agent_name(home.path(), "Mur"), "Mur");
    assert_eq!(canonicalize_agent_name(home.path(), "MUR"), "MUR");
}
