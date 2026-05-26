use std::process::Stdio;
use tempfile::TempDir;
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
        .env("MUR_AGENT_FORCE_ECHO", "1")
        .args(["--profile", "agent_t", "start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Drain stderr for debugging.
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut r = tokio::io::BufReader::new(stderr);
        let _ = tokio::io::copy(&mut r, &mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    });

    // Drain stdout; signal via oneshot when we receive the agent/card response.
    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel::<()>(1);
    let stdout_task = tokio::spawn(async move {
        let mut lines = Vec::new();
        let mut reader = tokio::io::BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim().to_string();
                    if trimmed.contains("\"result\"") {
                        let _ = ready_tx.try_send(());
                    }
                    lines.push(trimmed);
                }
                Err(_) => break,
            }
        }
        lines
    });

    // Send agent/card request to confirm the agent is fully booted.
    use tokio::io::AsyncWriteExt;
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"agent/card\"}\n")
        .await
        .unwrap();

    // Wait for the agent/card JSON-RPC response (skips telemetry lines).
    // The response proves the stdio transport is alive. Signal handlers are
    // installed BEFORE the transport spawns (supervisor.rs step 9→10), so
    // SIGTERM will be caught.
    tokio::time::timeout(std::time::Duration::from_secs(10), ready_rx.recv())
        .await
        .unwrap()
        .expect("agent/card response not received before channel closed");

    assert!(lock_path.exists(), "running.lock should exist while up");

    // Close stdin so serve_stdio sees EOF, avoiding pipe contention during
    // shutdown when the notification task may write to stdout.
    drop(stdin);

    #[cfg(unix)]
    unsafe {
        libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGTERM);
    }

    let exit_result = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait()).await;

    let stderr_output = stderr_task.await.unwrap_or_default();
    let _stdout = stdout_task.await.unwrap_or_default();

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
