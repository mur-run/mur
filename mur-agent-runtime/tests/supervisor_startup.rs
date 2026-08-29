use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

/// How long the runtime gets to answer `agent/card` from a cold start.
///
/// This only bounds a hang. A cold start answers in ~0.11s on an idle machine,
/// so the number is not a performance assertion and there is nothing to win by
/// keeping it small — a runtime that never answers still fails the test, just
/// later. It was 5s, which reads as generous at 45x the idle figure and still
/// expired at 5.020s during a full `nextest` run with clippy and a release
/// build alongside. Under `nextest` the whole workspace runs in parallel, so
/// that contention is this test's normal case, not an exceptional one.
const CARD_REPLY_BUDGET: Duration = Duration::from_secs(60);

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
    // Held so a startup crash can be reported with the runtime's own words
    // instead of `called Option::unwrap() on a None value`.
    let mut stderr = child.stderr.take().unwrap();
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
        // Three distinct failures used to collapse into three bare unwraps: a
        // timeout surfaced as `Elapsed(())`, and a runtime that died during
        // startup as `Option::unwrap() on a None value` — neither of which
        // names what went wrong, and the second is the one that most needs to.
        let line = match tokio::time::timeout(CARD_REPLY_BUDGET, reader.next_line()).await {
            Err(_) => panic!(
                "runtime did not answer agent/card within {CARD_REPLY_BUDGET:?} — \
                 a startup hang, or a cold start starved of CPU"
            ),
            Ok(Err(e)) => panic!("reading the runtime's stdout failed: {e}"),
            Ok(Ok(None)) => {
                use tokio::io::AsyncReadExt;
                let mut log = String::new();
                let _ = stderr.read_to_string(&mut log).await;
                panic!("runtime exited before answering agent/card. Its stderr:\n{log}");
            }
            Ok(Ok(Some(line))) => line,
        };
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
