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
    let stderr = child.stderr.take().unwrap();

    // Drain stdout continuously so the child's telemetry/json-rpc writes never
    // block on a full pipe buffer — especially during shutdown when the test
    // isn't reading synchronously.
    let mut stdout_lines = tokio::io::BufReader::new(stdout).lines();
    let stdout_task = tokio::spawn(async move {
        let mut lines = Vec::new();
        while let Ok(Some(line)) = stdout_lines.next_line().await {
            lines.push(line);
        }
        lines
    });

    // Drain stderr for debugging.
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut r = tokio::io::BufReader::new(stderr);
        let _ = tokio::io::copy(&mut r, &mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    });

    // Drive one request so we know the agent is fully up before SIGTERM.
    use tokio::io::AsyncWriteExt;
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"agent/card\"}\n")
        .await
        .unwrap();

    // Wait for the lock file — signals readiness sooner than reading stdout.
    let mut attempts = 0;
    while !lock_path.exists() && attempts < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        attempts += 1;
    }
    assert!(lock_path.exists(), "running.lock should exist while up");

    // Close stdin so serve_stdio sees EOF and exits its read loop, releasing
    // the stdout writer before the shutdown path tries to flush telemetry.
    // Without this, serve_stdio stays parked on stdin and the notification
    // task may contend for stdout, causing an intermittent pipe deadlock.
    drop(stdin);

    #[cfg(unix)]
    unsafe {
        libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGTERM);
    }

    let exit_result = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait()).await;

    let _stdout = stdout_task.await.unwrap_or_default();
    let stderr_output = stderr_task.await.unwrap_or_default();

    let exit_status = exit_result.unwrap_or_else(|_| {
        let _ = child.start_kill();
        panic!(
            "child process did not exit within 15s of SIGTERM\n\
             stderr: {stderr_output}"
        );
    });

    assert!(
        !lock_path.exists(),
        "running.lock must be deleted after SIGTERM\n\
         exit status: {exit_status:?}\n\
         stderr: {stderr_output}"
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
