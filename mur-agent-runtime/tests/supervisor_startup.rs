use std::process::Stdio;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

#[tokio::test]
async fn runtime_starts_and_responds_to_agent_card_over_stdio() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_t");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::write(
        agent_home.join("profile.yaml"),
        include_str!("fixtures/profile_stdio.yaml"),
    )
    .unwrap();
    std::fs::write(agent_home.join("sys_prompt.md"), "You are a test.").unwrap();

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

    use tokio::io::AsyncWriteExt;
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"agent/card\"}\n")
        .await
        .unwrap();

    // Skip JSON-RPC notifications (no `id`) until we find the response
    // to our agent/card request. Notifications now include the
    // `telemetry/hook_fired` events the A0 hook chain emits on startup.
    let resp = loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), reader.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v.get("id").is_some() {
            break v;
        }
    };
    assert_eq!(resp["result"]["name"], "agent_t");

    #[cfg(unix)]
    unsafe {
        libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGTERM);
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
}
