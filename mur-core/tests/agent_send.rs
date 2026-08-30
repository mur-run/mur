#![cfg(unix)]

use mur_common::{LockFile, agent::LockTransports};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::thread;
use tempfile::TempDir;

fn mur_create(mur_home: &Path, bin_dir: &Path, name: &str) {
    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home)
        .env("MUR_AGENT_BIN_DIR", bin_dir)
        .env("MUR_AGENT_RUNTIME_BIN", "/tmp/runtime-stub")
        .args(["agent", "create", name, "--no-interactive"])
        .output()
        .expect("spawn mur create");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_running_lock(mur_home: &Path, name: &str, sock: &str) {
    let lock_path = mur_home.join("agents").join(name).join("running.lock");
    let lock = LockFile {
        schema: 1,
        uuid: "0192f5a1-28ab-7111-8000-0000000000bb".into(),
        name: name.into(),
        pid: std::process::id(),
        ppid: 1,
        started_at: chrono::Utc::now().to_rfc3339(),
        binary_version: "mock".into(),
        transports: LockTransports {
            stdio: false,
            unix_socket: Some(sock.into()),
            tcp: None,
            webhook: None,
        },
        card_digest: "sha256:mock".into(),
        capabilities: vec!["a2a.message.send".into()],
        build_sha: String::new(),
        proto_version: 0,
        sandbox: None,
    };
    std::fs::write(lock_path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
}

fn spawn_mock_server(sock_path: std::path::PathBuf) {
    thread::spawn(move || {
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).expect("bind");
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let reader = BufReader::new(stream.try_clone().unwrap());
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let req: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = req["id"].clone();
                let method = req["method"].as_str().unwrap_or("");
                let result = match method {
                    "agent/card" => serde_json::json!({
                        "protocolVersion": "a2a/0.3",
                        "name": "agent_x",
                    }),
                    "message/send" => serde_json::json!({
                        "id": "task-mock",
                        "state": "completed",
                        "messages": [
                            req["params"]["message"].clone(),
                            {"role": "agent", "parts": [{"kind": "text", "text": "echo: ok"}]}
                        ],
                        "createdAt": chrono::Utc::now().to_rfc3339(),
                    }),
                    _ => serde_json::json!(null),
                };
                let resp =
                    serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string() + "\n";
                if stream.write_all(resp.as_bytes()).is_err() {
                    break;
                }
                let _ = stream.flush();
            }
        }
    });
    // Give the server a moment to bind.
    std::thread::sleep(std::time::Duration::from_millis(100));
}

#[test]
fn agent_send_roundtrips_message_over_unix_socket() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    let sock_dir = TempDir::new().unwrap();
    let sock_path = sock_dir.path().join("agent_x.sock");
    write_running_lock(mur_home.path(), "agent_x", sock_path.to_str().unwrap());
    spawn_mock_server(sock_path);

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args([
            "agent",
            "send",
            "agent_x",
            "{\"role\":\"user\",\"parts\":[{\"kind\":\"text\",\"text\":\"hi\"}]}",
        ])
        .output()
        .expect("spawn mur send");
    assert!(
        out.status.success(),
        "send failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["state"], "completed");
    assert_eq!(v["id"], "task-mock");
}

#[test]
fn agent_card_returns_agent_card() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    let sock_dir = TempDir::new().unwrap();
    let sock_path = sock_dir.path().join("agent_x.sock");
    write_running_lock(mur_home.path(), "agent_x", sock_path.to_str().unwrap());
    spawn_mock_server(sock_path);

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args(["agent", "card", "agent_x"])
        .output()
        .expect("spawn mur card");
    assert!(
        out.status.success(),
        "card failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(v["protocolVersion"], "a2a/0.3");
    assert_eq!(v["name"], "agent_x");
}
