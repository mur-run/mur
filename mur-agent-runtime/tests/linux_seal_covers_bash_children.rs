//! GHSA-j8mv-27gx-x7xj regression: on Linux, a `bash` child of a sealed
//! runtime must itself be confined.
//!
//! Landlock's `restrict_self` and a non-TSYNC seccomp filter confine only the
//! calling thread and the threads it creates afterwards. The runtime used to
//! build its multi-thread tokio runtime in `main` and seal later on one
//! thread, so every worker — and every `bash` child spawned from a worker —
//! ran unconfined: an agent could rewrite its own profile and entitlement
//! pin. The fix seals before the runtime exists.
//!
//! This drives the real binary end to end: start it over stdio and call the
//! `bash` tool through `tools/call` several times. Checks:
//!
//! - positive control: the child CAN write inside `agent_home` (always a
//!   write grant). Proves `bash` actually ran, so a refused call cannot pass
//!   the denial checks below by accident;
//! - behaviour, the real guard: the child cannot write the entitlement-pin
//!   directory (the exploit's target) or any other path outside its grants.
//!   The marker is assembled by `printf` at run time and read from the tool
//!   result's `content`, so it cannot match an echo of the command text;
//! - kernel state, read from outside: every thread of the runtime carries
//!   `NoNewPrivs: 1` and a seccomp filter. This only proves seccomp reached
//!   every thread. Since the filter is installed with TSYNC, it no longer
//!   catches a runtime built before the seal: those workers would still get
//!   seccomp, just not Landlock, and Linux exposes no per-thread Landlock
//!   flag to read. Ordering is guarded by the behaviour check above.
//!
//! Skips when Landlock is not enforcing on this kernel: there is no seal to
//! test, and the runtime's own fail-closed default refuses to start. Under
//! CI (`CI` set) it fails instead, because nextest hides the skip message
//! and a silent pass would look like coverage.
#![cfg(target_os = "linux")]

use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Hang bound only; a cold start answers in well under a second.
const REPLY_BUDGET: Duration = Duration::from_secs(60);
/// Denied-write calls made. Several calls make it likely, not certain, that
/// tokio schedules them on more than one worker thread; a heuristic.
const CALLS: u64 = 6;
/// Marker the child prints on a refused write, assembled at run time.
const DENIED: &str = "WRITE=denied";
/// Marker the child prints on a successful write.
const ALLOWED: &str = "WRITE=allowed";

const PROFILE: &str = r#"schema: 1
id: 0192f5a1-28ab-7111-8000-0000000000a3
name: agent_seal
display_name: "Agent Seal"
version: "0.1.0"
persona:
  category: research
  description: "Linux seal regression agent"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "m", params: {} }
mcp_servers: []
skills: []
transport: { stdio: true, socket: { enabled: false, bind: "unix:///nonexistent/agent_seal.sock" } }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: ["a2a.message.send","a2a.tasks"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: [] }
  processes: { spawn: { mode: any, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 64 }
  tools:
  - pattern: bash
    policy: allow
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 0, backoff: fixed, initial_delay_ms: 1000 }
  tool: { max_retries: 0, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: never, max_restarts: 0, restart_window_secs: 600, stop_timeout_secs: 5, mcp_required: false }
created_at: "2026-11-20T00:00:00+00:00"
updated_at: "2026-11-20T00:00:00+00:00"
"#;

/// Is Landlock available on this kernel? Asks the kernel for its ABI version
/// (`LANDLOCK_CREATE_RULESET_VERSION`) rather than reading
/// `/sys/kernel/security/lsm`, which a container usually does not mount.
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

#[tokio::test]
async fn bash_children_of_a_sealed_runtime_are_confined() {
    if !landlock_enforcing() {
        assert!(
            std::env::var_os("CI").is_none(),
            "Landlock is not available on this CI kernel, so the \
             GHSA-j8mv-27gx-x7xj regression test cannot run"
        );
        eprintln!("skipping: Landlock is not an active LSM on this kernel");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_seal");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::write(agent_home.join("profile.yaml"), PROFILE).unwrap();
    std::fs::write(agent_home.join("sys_prompt.md"), "You are a test.").unwrap();
    // Outside every grant: the pin directory the exploit rewrote, and a
    // plain sibling of the agents dir under MUR_HOME.
    let pins = tmp.path().join("entitlement-pins");
    let outside = tmp.path().join("outside");
    for d in [&pins, &outside] {
        std::fs::create_dir_all(d).unwrap();
    }

    let bin = env!("CARGO_BIN_EXE_mur-agent-runtime");
    let mut child = Command::new(bin)
        .env("MUR_HOME", tmp.path())
        .args(["--profile", "agent_seal", "start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let stderr = child.stderr.take().unwrap();

    let mut session = Session {
        stdin,
        lines,
        stderr,
    };

    // Positive control: a write inside agent_home must succeed.
    let granted = agent_home.join("seal-positive-control");
    let out = session.bash(0, &granted).await;
    assert!(
        out.contains(ALLOWED),
        "positive control: bash could not write inside agent_home, so it \
         probably never ran and the denial checks would prove nothing. \
         Tool output: {out}"
    );
    assert!(
        granted.exists(),
        "positive control: {} was not created",
        granted.display()
    );

    for id in 1..=CALLS {
        let dir = if id % 2 == 0 { &pins } else { &outside };
        let target = dir.join(format!("agent_seal-{id}.json"));
        let out = session.bash(id, &target).await;
        assert!(
            out.contains(DENIED),
            "call {id}: bash child wrote outside every grant — Landlock did \
             not confine it (GHSA-j8mv-27gx-x7xj). Tool output: {out}"
        );
        assert!(
            !target.exists(),
            "call {id}: {} was created by the bash child",
            target.display()
        );
    }

    let pid = child.id().unwrap();
    let unsealed = unsealed_threads(pid);
    assert!(
        unsealed.is_empty(),
        "runtime threads without NO_NEW_PRIVS or a seccomp filter — the seal \
         ran after they existed (GHSA-j8mv-27gx-x7xj): {unsealed:?}"
    );

    unsafe {
        libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGTERM);
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}

/// The runtime's stdio, driven one `tools/call` at a time.
struct Session {
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    stderr: tokio::process::ChildStderr,
}

impl Session {
    /// Ask `bash` to create `target` and return the tool result's text. The
    /// marker is built by `printf` from two arguments, so the literal
    /// `WRITE=denied` / `WRITE=allowed` never appears in the command itself.
    /// No `2>/dev/null`: the sandbox grants no write on `/dev/null` here, so
    /// that redirect alone failed every call and made the denial vacuous
    /// (the positive control is what exposed it). The shell's error text
    /// just lands in the tool output.
    async fn bash(&mut self, id: u64, target: &std::path::Path) -> String {
        let cmd = format!(
            "if : > '{}'; then printf '%s=%s\\n' WRITE allowed; \
             else printf '%s=%s\\n' WRITE denied; fi",
            target.display()
        );
        let req = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {
                "task_id": format!("seal-{id}"),
                "name": "bash",
                "arguments": { "command": cmd },
            }
        });
        self.stdin
            .write_all(format!("{req}\n").as_bytes())
            .await
            .unwrap();

        let resp = loop {
            let line = match tokio::time::timeout(REPLY_BUDGET, self.lines.next_line()).await {
                Err(_) => panic!("runtime did not answer tools/call within {REPLY_BUDGET:?}"),
                Ok(Err(e)) => panic!("reading the runtime's stdout failed: {e}"),
                Ok(Ok(None)) => {
                    use tokio::io::AsyncReadExt;
                    let mut log = String::new();
                    let _ = self.stderr.read_to_string(&mut log).await;
                    panic!("runtime exited before answering. Its stderr:\n{log}");
                }
                Ok(Ok(Some(l))) => l,
            };
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                break v;
            }
        };
        let result = &resp["result"];
        assert_eq!(
            result["is_error"],
            serde_json::json!(false),
            "call {id}: the bash tool call itself failed: {resp}"
        );
        match &result["content"] {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

/// Threads of `pid` missing `NoNewPrivs: 1` or `Seccomp: 2`, as
/// `(comm, NoNewPrivs, Seccomp)`. Empty when every thread is sealed.
fn unsealed_threads(pid: u32) -> Vec<(String, String, String)> {
    let mut bad = Vec::new();
    for t in std::fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        let status = match std::fs::read_to_string(t.unwrap().path().join("status")) {
            Ok(s) => s,
            Err(_) => continue, // the thread exited between readdir and read
        };
        let field = |k: &str| {
            status
                .lines()
                .find_map(|l| l.strip_prefix(k))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        let (nnp, seccomp) = (field("NoNewPrivs:"), field("Seccomp:"));
        if nnp != "1" || seccomp != "2" {
            bad.push((field("Name:"), nnp, seccomp));
        }
    }
    bad
}
