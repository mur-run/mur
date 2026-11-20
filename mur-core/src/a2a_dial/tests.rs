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

// ---------------------------------------------------------------------------
// match_agent_name: the pure selection both resolvers share (#2205 review)
// ---------------------------------------------------------------------------

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn match_agent_name_exact_wins_over_case_variants() {
    let n = names(&["Reviewer", "reviewer"]);
    assert_eq!(
        match_agent_name(&n, "reviewer"),
        NameMatch::Found("reviewer".into())
    );
    assert_eq!(
        match_agent_name(&n, "Reviewer"),
        NameMatch::Found("Reviewer".into())
    );
}

#[test]
fn match_agent_name_single_fold_resolves() {
    let n = names(&["reviewer", "mur"]);
    assert_eq!(
        match_agent_name(&n, "REVIEWER"),
        NameMatch::Found("reviewer".into())
    );
}

/// The bug: with two case variants and no exact match the first `read_dir`
/// entry used to win. Both orders must report ambiguity.
#[test]
fn match_agent_name_several_folds_is_ambiguous_in_any_order() {
    for order in [["Reviewer", "reviewer"], ["reviewer", "Reviewer"]] {
        match match_agent_name(&names(&order), "REVIEWER") {
            NameMatch::Ambiguous(mut c) => {
                c.sort();
                assert_eq!(c, names(&["Reviewer", "reviewer"]));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }
}

#[test]
fn match_agent_name_missing() {
    assert_eq!(
        match_agent_name(&names(&["mur"]), "ghost"),
        NameMatch::Missing
    );
}

// ---------------------------------------------------------------------------
// Owner proof on owner methods (#2488)
// ---------------------------------------------------------------------------

/// A running agent `name` whose socket records the one request it gets and
/// answers it. Returns the captured request.
#[cfg(unix)]
fn capture_one(home: &std::path::Path, name: &str, method: &str) -> Value {
    use std::io::{BufRead, BufReader, Write};
    let adir = home.join("agents").join(name);
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::write(adir.join("profile.yaml"), format!("name: {name}\n")).unwrap();
    // sun_path is short on macOS; keep the socket path brief.
    let sdir = tempfile::Builder::new()
        .prefix("oc")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = sdir.path().join("s");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let lock = serde_json::json!({
        "schema": 1, "uuid": "u", "name": name, "pid": std::process::id(), "ppid": 1,
        "started_at": "t", "binary_version": "v",
        "proto_version": mur_common::build::A2A_PROTO_VERSION,
        "transports": {"stdio": false, "unix_socket": sock.to_str().unwrap()},
        "card_digest": "d", "capabilities": []
    });
    std::fs::write(adir.join("running.lock"), lock.to_string()).unwrap();
    let srv = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(s.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let req: Value = serde_json::from_str(&line).unwrap();
        let resp = serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": {}});
        (&s).write_all(format!("{resp}\n").as_bytes()).unwrap();
        req
    });
    dial_method(home, name, method, Value::Null, DialMode::RequireRunning).unwrap();
    let req = srv.join().unwrap();
    drop(sdir);
    req
}

#[cfg(unix)]
#[test]
fn owner_methods_carry_a_proof_bound_to_the_agent_and_never_the_token() {
    let home = tempdir().unwrap();
    let token = mur_common::hitl::approval_token::load_or_create(home.path()).unwrap();
    let req = capture_one(home.path(), "alice", "tasks/cancel");
    let sent = req["params"][mur_common::owner_call::PARAM]
        .as_str()
        .expect("owner proof attached");
    assert!(mur_common::owner_call::verify(
        &token,
        "alice",
        "tasks/cancel",
        sent
    ));
    assert!(!req.to_string().contains(&token), "token must not travel");
}

#[cfg(unix)]
#[test]
fn peer_methods_carry_no_owner_proof() {
    let home = tempdir().unwrap();
    mur_common::hitl::approval_token::load_or_create(home.path()).unwrap();
    let req = capture_one(home.path(), "alice", "agent/card");
    assert!(
        req["params"].get(mur_common::owner_call::PARAM).is_none(),
        "{req}"
    );
}
