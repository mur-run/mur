//! `mur agent cli --plain` with piped stdin, through the real binary (#2563):
//! slash commands never reach the agent, and `/quit` ends the session even
//! with more input still waiting in the pipe.

#![cfg(unix)]

use mur_common::{LockFile, agent::LockTransports};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const AGENT: &str = "agent_x";
/// Far above a cold binary start on a loaded CI runner (~30s seen locally);
/// a real hang never ends, so a generous bound costs nothing.
const EXIT_DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

fn mur_create(mur_home: &Path, bin_dir: &Path) {
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .env("MUR_HOME", mur_home)
        .env("MUR_AGENT_BIN_DIR", bin_dir)
        .env("MUR_AGENT_RUNTIME_BIN", "/nonexistent/runtime-stub")
        .args(["agent", "create", AGENT, "--no-interactive"])
        .output()
        .expect("spawn mur create");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Mark the agent live on `sock`, so `murmur` dials it instead of starting it.
fn write_running_lock(mur_home: &Path, sock: &Path) {
    let lock = LockFile {
        schema: 1,
        uuid: "0192f5a1-28ab-7111-8000-0000000000cc".into(),
        name: AGENT.into(),
        pid: std::process::id(),
        ppid: 1,
        started_at: chrono::Utc::now().to_rfc3339(),
        binary_version: "mock".into(),
        transports: LockTransports {
            stdio: false,
            unix_socket: Some(sock.to_string_lossy().into()),
            tcp: None,
            webhook: None,
        },
        card_digest: "sha256:mock".into(),
        capabilities: vec!["a2a.message.send".into()],
        build_sha: String::new(),
        proto_version: 0,
        sandbox: None,
        startup_override: None,
        override_rejected: None,
    };
    let path = mur_home.join("agents").join(AGENT).join("running.lock");
    std::fs::write(path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
}

/// An agent socket that records every method it is asked for and answers
/// nothing useful: the test passes only if nothing is asked at all.
fn spawn_recorder(sock: PathBuf) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(&sock).expect("bind");
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let reader = BufReader::new(stream.try_clone().unwrap());
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let req: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
                log.lock()
                    .unwrap()
                    .push(req["method"].as_str().unwrap_or("?").to_string());
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req["id"],
                    "error": { "code": -32601, "message": "recorder" },
                });
                if writeln!(stream, "{resp}").is_err() {
                    break;
                }
            }
        }
    });
    seen
}

#[test]
fn piped_slash_commands_and_quit_never_reach_the_agent() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    let bin_dir = TempDir::new().unwrap();
    mur_create(&mur_home, bin_dir.path());
    let sock = tmp.path().join("agent.sock");
    write_running_lock(&mur_home, &sock);
    let seen = spawn_recorder(sock);

    let mut child = Command::new(env!("CARGO_BIN_EXE_mur"))
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .current_dir(tmp.path())
        .args(["agent", "cli", AGENT, "--plain"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn murmur --plain");
    // The line after `/quit` must never be read, let alone sent.
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/channels\n/quit\nhello after quit\n")
        .unwrap();

    let deadline = Instant::now() + EXIT_DEADLINE;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("murmur --plain did not exit after /quit");
        }
        std::thread::sleep(POLL);
    }
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("slash commands need the TUI"),
        "the held-back /channels must say why, on stderr: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "stdout is the reply stream; nothing was replied: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().all(|m| !m.starts_with("message/")),
        "no message may reach the agent, got {seen:?}"
    );
}
