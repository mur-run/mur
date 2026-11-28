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

/// Variables the child may inherit from the developer's shell: enough to
/// find tools and scratch space, nothing that steers MUR (`MUR_*`, `RUST_LOG`,
/// `NO_COLOR`, `TERM`…), so a run behaves the same on every machine.
const INHERITED_ENV: &[&str] = &["PATH", "TMPDIR"];

/// The real `mur` binary with a cleared environment, pointed at the test's
/// own home, MUR home and agent bin dir.
fn mur(mur_home: &Path, home: &Path, bin_dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    cmd.env_clear();
    for key in INHERITED_ENV {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd.env("MUR_HOME", mur_home)
        .env("HOME", home)
        .env("MUR_AGENT_BIN_DIR", bin_dir)
        .current_dir(home);
    cmd
}

fn mur_create(mur_home: &Path, home: &Path, bin_dir: &Path) {
    let out = mur(mur_home, home, bin_dir)
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
    mur_create(&mur_home, tmp.path(), bin_dir.path());
    let sock = tmp.path().join("agent.sock");
    write_running_lock(&mur_home, &sock);
    let seen = spawn_recorder(sock);

    let mut child = mur(&mur_home, tmp.path(), bin_dir.path())
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

/// A turn's `usage` as a runtime that predates `by_model` reports it: all
/// 2000 tokens attributed to the one answering model.
fn usage_of(model: &str) -> serde_json::Value {
    serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000, "model_ref": model })
}

/// An agent socket that answers each `message/send` with a finished task
/// carrying the next entry of `usages`, as a runtime that fell back would.
/// Anything else gets an error, as in [`spawn_recorder`].
fn spawn_scripted(sock: PathBuf, usages: Vec<serde_json::Value>) {
    let listener = UnixListener::bind(&sock).expect("bind");
    std::thread::spawn(move || {
        let mut turn = 0usize;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let reader = BufReader::new(stream.try_clone().unwrap());
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let req: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
                let resp = if req["method"] == "message/send" && turn < usages.len() {
                    turn += 1;
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req["id"],
                        "result": {
                            "id": format!("task-{turn}"),
                            "state": "completed",
                            "messages": [{ "role": "agent", "parts": [{ "text": format!("reply {turn}") }] }],
                            "usage": usages[turn - 1],
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
    mur_create(&mur_home, tmp.path(), bin_dir.path());
    std::fs::write(mur_home.join("models.yaml"), REGISTRY).unwrap();
    let profile = mur_home.join("agents").join(AGENT).join("profile.yaml");
    let mut yaml: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&profile).unwrap()).unwrap();
    yaml.as_mapping_mut()
        .expect("profile.yaml is a mapping")
        .insert("model_ref".into(), "metered".into());
    std::fs::write(&profile, serde_yaml::to_string(&yaml).unwrap()).unwrap();
    let sock = tmp.path().join("agent.sock");
    write_running_lock(&mur_home, &sock);
    // configured → fallback → same fallback → unknown → fallback again →
    // a mixed turn a current runtime splits by answering model.
    spawn_scripted(
        sock,
        vec![
            usage_of("metered-model"),
            usage_of("sub-model"),
            usage_of("sub-model"),
            usage_of("mystery-model"),
            usage_of("sub-model"),
            serde_json::json!({
                "input_tokens": 2000,
                "output_tokens": 2000,
                "model_ref": "sub-model",
                "by_model": [
                    { "registry_key": "metered", "model": "metered-model",
                      "input_tokens": 1000, "output_tokens": 1000 },
                    { "registry_key": "sub", "model": "sub-model",
                      "input_tokens": 1000, "output_tokens": 1000 },
                ],
            }),
        ],
    );

    let mut child = mur(&mur_home, tmp.path(), bin_dir.path())
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
        .write_all(b"one\ntwo\nthree\nfour\nfive\nsix\n")
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

    // Each turn's block — its reply up to the next turn's — must carry exactly
    // its own notice and footer, so a footer that matches on one turn cannot
    // cover for a wrong one on another (#2632 review).
    let notice = "⇄ answered by 'sub' (sub-model), not 'metered' — the runtime fell back. \
                  'sub' is subscription: it adds nothing per token.";
    let expected: [(&str, &[&str]); 6] = [
        // Configured metered model: priced at its own rates.
        ("reply 1", &["  2000 tok · $0.018"]),
        // Fallback to the subscription model: announced, labelled, not priced.
        ("reply 2", &[notice, "  2000 tok · subscription"]),
        // Same fallback again: no second notice.
        ("reply 3", &["  2000 tok · subscription"]),
        // Unknown model: fail-closed upper estimate at the configured rates (#2620).
        (
            "reply 4",
            &["  2000 tok · ≤$0.018 upper estimate, no rate for mystery-model"],
        ),
        // Back to the fallback after the unknown model: announced again.
        ("reply 5", &[notice, "  2000 tok · subscription"]),
        // Mixed turn reported under the subscription model: its metered share
        // is still billed, priced at its own rates (C1, `cost_by_share`).
        ("reply 6", &["  4000 tok · $0.018"]),
    ];
    let starts: Vec<usize> = expected
        .iter()
        .map(|(reply, _)| {
            stdout
                .find(reply)
                .unwrap_or_else(|| panic!("{reply} missing: {stdout}"))
        })
        .collect();
    for (i, (reply, lines)) in expected.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(stdout.len());
        assert!(starts[i] < end, "turns out of order: {stdout}");
        let block: Vec<&str> = stdout[starts[i]..end]
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert_eq!(&block, lines, "{reply} block: {stdout}");
    }
    assert!(!stdout[..starts[0]].contains('⇄'), "{stdout}");
}
