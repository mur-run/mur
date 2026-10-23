use super::*;

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn build_commands_want_the_server() {
    for c in [
        "cargo build",
        "cd x && cargo nextest run --workspace",
        "RUSTC_WRAPPER= /usr/local/bin/cargo test",
        "(cargo clippy)|tail -2",
        "sccache --show-stats",
    ] {
        assert!(wants_server(c), "{c}");
    }
}

#[test]
fn other_commands_do_not() {
    for c in ["ls -la", "git log -1", "echo cargo-ish", "cat Cargo.toml"] {
        assert!(!wants_server(c), "{c}");
    }
}

/// A word match, not a parse: `cargo` as an argument also counts. The cost
/// is one socket probe (and at most one idle server), which is why a looser
/// match is preferred over missing a real build.
#[test]
fn cargo_as_an_argument_also_counts() {
    assert!(wants_server("grep -rn cargo docs"));
}

#[test]
fn the_last_socket_entry_wins() {
    let e = env(&[
        ("SCCACHE_SERVER_UDS", "/a"),
        ("X", "1"),
        ("SCCACHE_SERVER_UDS", "/b"),
    ]);
    assert_eq!(socket_from_env(&e), Some("/b"));
    assert_eq!(socket_from_env(&env(&[("X", "1")])), None);
}

/// #1860 follow-up, end to end with the real sccache: after `ensure_for` a server is
/// listening on the child's socket, so parallel compiles find it instead of
/// racing to start one. Skipped where sccache is not installed.
#[tokio::test]
async fn ensure_for_leaves_a_server_listening_on_the_socket() {
    let Ok(path) = std::env::var("PATH") else {
        return;
    };
    if !std::env::split_paths(&path).any(|d| d.join("sccache").is_file()) {
        eprintln!("sccache not on PATH; skipping");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let sock = d.path().join("s.sock");
    let sock_s = sock.to_string_lossy().into_owned();
    let mut e = env(&[("PATH", &path), ("SCCACHE_SERVER_UDS", &sock_s)]);
    e.push((
        "SCCACHE_DIR".into(),
        d.path().join("c").to_string_lossy().into_owned(),
    ));
    assert!(std::os::unix::net::UnixStream::connect(&sock).is_err());

    ensure_for("cargo build", &e).await;
    assert!(
        std::os::unix::net::UnixStream::connect(&sock).is_ok(),
        "no server on {sock_s}"
    );

    let _ = std::process::Command::new("sccache")
        .arg("--stop-server")
        .env("SCCACHE_SERVER_UDS", &sock)
        .output();
}

#[tokio::test]
async fn a_non_build_command_starts_nothing() {
    let d = tempfile::tempdir().unwrap();
    let sock = d.path().join("s.sock");
    let e = env(&[("SCCACHE_SERVER_UDS", &sock.to_string_lossy())]);
    ensure_for("ls", &e).await;
    assert!(!sock.exists());
}
