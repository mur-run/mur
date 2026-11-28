//! `mur agent cli --plain` with piped stdin, through the real binary (#2563):
//! slash commands never reach the agent, and `/quit` ends the session even
//! with more input still waiting in the pipe. Also the per-turn fallback
//! notice and usage footer, end to end (#2625 review).

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

const REGISTRY: &str = r#"
schema_version: 1
models:
  metered:
    provider: anthropic
    model: metered-model
    input_cost_per_1k: 0.003
    output_cost_per_1k: 0.015
  sub:
    provider: openai
    model: sub-model
    billing: subscription
"#;

/// An agent socket that answers each `message/send` with a finished task
/// whose `usage.model_ref` is the next entry of `models`, as a runtime that
/// fell back would. Anything else gets an error, as in [`spawn_recorder`].
fn spawn_scripted(sock: PathBuf, models: &'static [&'static str]) {
    let listener = UnixListener::bind(&sock).expect("bind");
    std::thread::spawn(move || {
        let mut turn = 0usize;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let reader = BufReader::new(stream.try_clone().unwrap());
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let req: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
                let resp = if req["method"] == "message/send" && turn < models.len() {
                    turn += 1;
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req["id"],
                        "result": {
                            "id": format!("task-{turn}"),
                            "state": "completed",
                            "messages": [{ "role": "agent", "parts": [{ "text": format!("reply {turn}") }] }],
                            "usage": {
                                "input_tokens": 1000,
                                "output_tokens": 1000,
                                "model_ref": models[turn - 1],
                            },
                        },
                    })
                } else {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req["id"],
                        "error": { "code": -32601, "message": "scripted" },
                    })
                };
                if writeln!(stream, "{resp}").is_err() {
                    break;
                }
            }
        }
    });
}

/// Through the real binary: `models.yaml` + the profile's `model_ref` feed
/// the pricing book, and each piped turn prints its reply, a fallback notice
/// only when the answering model changes away from the configured one, and
/// a footer priced for the model that actually answered.
#[test]
fn plain_announces_each_fallback_once_and_prices_the_answering_model() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    let bin_dir = TempDir::new().unwrap();
    mur_create(&mur_home, bin_dir.path());
    std::fs::write(mur_home.join("models.yaml"), REGISTRY).unwrap();
    let profile = mur_home.join("agents").join(AGENT).join("profile.yaml");
    let mut yaml = std::fs::read_to_string(&profile).unwrap();
    yaml.push_str("model_ref: metered\n");
    std::fs::write(&profile, yaml).unwrap();
    let sock = tmp.path().join("agent.sock");
    write_running_lock(&mur_home, &sock);
    // configured → fallback → same fallback → unknown → fallback again.
    spawn_scripted(
        sock,
        &[
            "metered-model",
            "sub-model",
            "sub-model",
            "mystery-model",
            "sub-model",
        ],
    );

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
    // Dropping stdin after the write is the EOF that ends the session.
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"one\ntwo\nthree\nfour\nfive\n")
        .unwrap();

    let deadline = Instant::now() + EXIT_DEADLINE;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("murmur --plain did not exit at end of input");
        }
        std::thread::sleep(POLL);
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");

    for n in 1..=5 {
        assert!(
            stdout.contains(&format!("reply {n}")),
            "turn {n} missing: {stdout}"
        );
    }
    let notices: Vec<&str> = stdout.lines().filter(|l| l.starts_with('⇄')).collect();
    assert_eq!(
        notices.len(),
        2,
        "announced on turn 2 and again on turn 5, after the unknown model: {stdout}"
    );
    for n in &notices {
        assert!(
            n.starts_with("⇄ answered by 'sub' (sub-model), not 'metered'"),
            "{n}"
        );
    }
    // The configured metered model is priced; the subscription fallback is
    // labelled, never priced at the configured model's rates.
    assert!(stdout.contains("  2000 tok · $0.018"), "{stdout}");
    assert!(stdout.contains("  2000 tok · subscription"), "{stdout}");
    // The notice sits between turn 2's reply and its footer.
    let r2 = stdout.find("reply 2").unwrap();
    let n2 = stdout.find('⇄').unwrap();
    assert!(r2 < n2 && !stdout[..r2].contains('⇄'), "{stdout}");
}
