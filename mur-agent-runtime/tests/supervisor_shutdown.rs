use std::process::Stdio;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

// Unix-only: exercises SIGTERM → supervisor shutdown → lock cleanup.
// Windows has no SIGTERM equivalent; without the signal the supervisor never
// shuts down cleanly within the test timeout, leaving running.lock on disk
// and the assertions unsatisfiable.
#[cfg(unix)]
#[tokio::test]
async fn sigterm_removes_running_lock_and_flushes_telemetry() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_t");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::write(
        agent_home.join("profile.yaml"),
        include_str!("fixtures/profile_stdio.yaml"),
    )
    .unwrap();
    std::fs::write(agent_home.join("sys_prompt.md"), "You are a test.").unwrap();
    let lock_path = agent_home.join("running.lock");

    let bin = env!("CARGO_BIN_EXE_mur-agent-runtime");
    let mut child = Command::new(bin)
        .env("MUR_HOME", tmp.path())
        .args(["--profile", "agent_t", "start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = tokio::io::BufReader::new(stdout).lines();

    // Drive one request so we know the agent is fully up before SIGTERM.
    use tokio::io::AsyncWriteExt;
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"agent/card\"}\n")
        .await
        .unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), reader.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert!(lock_path.exists(), "running.lock should exist while up");

    #[cfg(unix)]
    unsafe {
        libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGTERM);
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait()).await;

    assert!(
        !lock_path.exists(),
        "running.lock must be deleted after SIGTERM"
    );

    // Telemetry JSONL file for today should contain a shutdown warning.
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let telem = agent_home.join("telemetry").join(format!("{today}.jsonl"));
    let body = std::fs::read_to_string(&telem).unwrap_or_default();
    assert!(
        body.contains("\"kind\":\"shutdown\""),
        "telemetry must contain shutdown event, got: {body}"
    );
}
