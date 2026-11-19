//! #2422 regression: the REAL sealed runtime must come up on a unix socket
//! whose bind path lies outside every Landlock write grant.
//!
//! Before the fix the runtime bound after `prepare_and_seal`, so Landlock
//! refused `MAKE_SOCK` for both paths below and the agent ran socket-less.
//! These tests start the binary, so reverting to a post-seal bind fails them:
//!
//! - long `agent_home`: the `/tmp/mur-<uuid>.sock` fallback; the client dials
//!   the canonical path through the symlink (via a relative path from its
//!   directory, since the absolute one exceeds `sun_path`);
//! - a configured `transport.socket.bind` outside `agent_home`;
//! - an impossible bind path: startup exits naming the path.
//!
//! Skips when Landlock is unavailable; fails instead under CI (`CI` set), as
//! `linux_seal_covers_bash_children.rs` does.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

/// Hang bound only; a cold start answers in well under a second.
const BUDGET: Duration = Duration::from_secs(60);
/// Exceeds the runtime's `MAX_SAFE_PATH_BYTES` (100) with room to spare.
const LONG_SEGMENT: usize = 120;
const CARD_REQ: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"agent/card\"}\n";

fn profile(bind: &Path) -> String {
    format!(
        r##"schema: 1
id: 0192f5a1-28ab-7111-8000-0000000000b4
name: agent_sock
display_name: "Agent Sock"
version: "0.1.0"
persona:
  category: research
  description: "#2422 socket-before-seal regression agent"
  traits: {{ tone: concise, risk: cautious, verbosity: low }}
sys_prompt_file: "sys_prompt.md"
model: {{ provider: ollama, name: "m", params: {{}} }}
mcp_servers: []
skills: []
transport: {{ stdio: true, socket: {{ enabled: true, bind: "unix://{}" }} }}
communication: {{ accepts_from: ["*"], sends_to: [] }}
capabilities: ["a2a.message.send","a2a.tasks"]
entitlements:
  network:
    inbound: {{ ports: [] }}
    outbound: {{ mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: {{ mode: system }} }}
  filesystem: {{ read: [], write: [], deny: [] }}
  processes: {{ spawn: {{ mode: allowlist, allowed: [] }} }}
  syscalls: {{ mode: default }}
  limits: {{ memory_mb: 512, file_descriptors: 1024, processes: 32 }}
notifications: {{ on_task_complete: [], on_error: [], on_shutdown: [] }}
retry:
  llm: {{ max_retries: 0, backoff: fixed, initial_delay_ms: 1000 }}
  tool: {{ max_retries: 0, backoff: fixed, initial_delay_ms: 500 }}
lifecycle: {{ restart: never, max_restarts: 0, restart_window_secs: 600, stop_timeout_secs: 5, mcp_required: false }}
created_at: "2026-11-20T00:00:00+00:00"
updated_at: "2026-11-20T00:00:00+00:00"
"##,
        bind.display()
    )
}

/// Same probe as `linux_seal_covers_bash_children.rs`.
fn landlock_enforcing() -> bool {
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_long = 1;
    // SAFETY: the documented version query — null attr, size 0, flag 1.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    abi >= 1
}

/// `false` = skip (local kernel without Landlock); panics under CI.
fn landlock_or_skip() -> bool {
    if landlock_enforcing() {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "Landlock is not available on this CI kernel, so the #2422 test cannot run"
    );
    eprintln!("skipping: Landlock is not an active LSM on this kernel");
    false
}

/// Write an `agent_sock` home under `mur_home` binding `bind`, and start it.
fn start(mur_home: &Path, agent_home: &Path, bind: &Path) -> Child {
    std::fs::create_dir_all(agent_home).unwrap();
    std::fs::write(agent_home.join("profile.yaml"), profile(bind)).unwrap();
    std::fs::write(agent_home.join("sys_prompt.md"), "You are a test.").unwrap();
    assert_eq!(agent_home, mur_home.join("agents").join("agent_sock"));
    Command::new(env!("CARGO_BIN_EXE_mur-agent-runtime"))
        .env("MUR_HOME", mur_home)
        .args(["--profile", "agent_sock", "start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

/// Dial `dial` (retrying while the runtime boots) and ask for the card.
async fn card_over_socket(child: &mut Child, dial: &Path) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + BUDGET;
    let stream = loop {
        if let Ok(s) = UnixStream::connect(dial).await {
            break s;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let mut log = String::new();
            let _ = child.stderr.take().unwrap().read_to_string(&mut log).await;
            panic!("runtime exited ({status}) before its socket came up:\n{log}");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "socket {} never accepted within {BUDGET:?}",
            dial.display()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let (read, mut write) = stream.into_split();
    write.write_all(CARD_REQ).await.unwrap();
    let mut lines = BufReader::new(read).lines();
    loop {
        let line = tokio::time::timeout(BUDGET, lines.next_line())
            .await
            .expect("no reply on the socket")
            .unwrap()
            .expect("socket closed before replying");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v.get("id") == Some(&serde_json::json!(7)) {
            return v;
        }
    }
}

fn assert_card(v: &serde_json::Value) {
    assert!(v.get("error").is_none(), "agent/card failed: {v}");
    assert_eq!(v["result"]["name"], "agent_sock", "unexpected card: {v}");
}

#[tokio::test]
async fn sealed_runtime_answers_on_its_canonical_path_with_a_long_agent_home() {
    if !landlock_or_skip() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let mur_home: PathBuf = tmp.path().join("a".repeat(LONG_SEGMENT));
    let agent_home = mur_home.join("agents").join("agent_sock");
    let canonical = agent_home.join("agent.sock");
    let mut child = start(&mur_home, &agent_home, &canonical);

    // The absolute canonical path is too long for sun_path; dial it relative
    // to its directory so the kernel follows the runtime's symlink. Each
    // nextest test is its own process, so changing cwd is local to it.
    std::env::set_current_dir(&agent_home).unwrap();
    let v = card_over_socket(&mut child, Path::new("agent.sock")).await;
    assert_card(&v);
    let target = std::fs::read_link(&canonical).expect("canonical path is a symlink");
    assert!(
        target.starts_with("/tmp"),
        "fallback must live in /tmp, outside every grant: {}",
        target.display()
    );
}

#[tokio::test]
async fn sealed_runtime_answers_on_a_configured_socket_outside_agent_home() {
    if !landlock_or_skip() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_sock");
    // A sibling of `agents/` under MUR_HOME: outside every write grant.
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let bind = outside.join("s.sock");
    let mut child = start(tmp.path(), &agent_home, &bind);
    assert_card(&card_over_socket(&mut child, &bind).await);
}

#[tokio::test]
async fn sealed_runtime_refuses_to_start_on_an_unbindable_socket() {
    if !landlock_or_skip() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_sock");
    let bind = tmp.path().join("missing-dir").join("s.sock");
    let mut child = start(tmp.path(), &agent_home, &bind);
    let status = tokio::time::timeout(BUDGET, child.wait())
        .await
        .expect("runtime kept running with no socket")
        .unwrap();
    assert!(!status.success(), "startup must fail");
    let mut log = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut log)
        .await
        .unwrap();
    assert!(
        log.contains("cannot bind the agent's unix socket") && log.contains("missing-dir"),
        "error must name the path:\n{log}"
    );
}
