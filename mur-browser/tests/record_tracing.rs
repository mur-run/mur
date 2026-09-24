//! Tracing trail for `mur browser record`.
//!
//! Moved out of the lib unit tests into its own test binary: there, the
//! `session ended` event (logged in `proxy.rs`) went missing intermittently
//! while other `run_io` tests ran in parallel without a subscriber. The
//! likely cause is `tracing`'s process-wide callsite-interest cache, which a
//! thread-local `set_default` subscriber does not reliably win against.
//! A binary with a single test has no neighbours to race.

use mur_browser::proxy::run_io;
use mur_browser::recorder::{Mode, RecordHook, Run};
use serde_json::Value;

/// Captures `tracing` output for one test; `#[tokio::test]` is
/// current-thread, so a thread-local default covers spawned tasks too.
#[derive(Clone, Default)]
struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// `mur browser record` must leave a trail: which steps were written,
/// which were rejected and why, and how the session ended. Before this,
/// `RUST_LOG=mur_browser=debug` produced a 0-byte log for a whole run.
#[tokio::test]
async fn record_session_emits_tracing_for_steps_rejects_and_shutdown() {
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};

    let logs = LogBuf::default();
    let sink = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("mur_browser=debug")
        .with_ansi(false)
        .with_writer(move || sink.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    async fn server(mut input: impl AsyncRead + Unpin, mut output: impl AsyncWrite + Unpin) {
        let mut lines = BufReader::new(&mut input).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"].clone(),
                "result": {"content": [{"type": "text", "text": "ok"}]}
            });
            let _ = output.write_all(format!("{response}\n").as_bytes()).await;
            let _ = output.flush().await;
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let actions = dir.path().join("actions.yaml");
    let hook = RecordHook::with_actions_path(
        Run {
            name: "traced".into(),
            mode: Mode::Test,
            profile: None,
            recorded_at: chrono::Utc::now(),
            steps: vec![],
        },
        actions,
    );
    let (mut agent_write, agent_input) = duplex(16 * 1024);
    let (agent_output, mut agent_read) = duplex(16 * 1024);
    let (server_input, server_read) = duplex(16 * 1024);
    let (server_write, server_output) = duplex(16 * 1024);
    tokio::spawn(server(server_read, server_write));
    let session = tokio::spawn(run_io(
        agent_input,
        agent_output,
        server_input,
        server_output,
        hook,
    ));

    let mut lines = BufReader::new(&mut agent_read).lines();
    // Recorded: navigation needs no locator.
    agent_write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_navigate\",\"arguments\":{\"url\":\"https://example.test\"}}}\n").await.unwrap();
    lines.next_line().await.unwrap().unwrap();
    // Rejected: a bare @ref click (no element text, no snapshot) has no stable locator.
    agent_write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_click\",\"arguments\":{\"ref\":\"e9\"}}}\n").await.unwrap();
    let rejected: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert!(
        rejected.get("error").is_some(),
        "click must be rejected: {rejected}"
    );
    drop(agent_write);
    tokio::time::timeout(std::time::Duration::from_secs(2), session)
        .await
        .expect("session did not end")
        .unwrap()
        .unwrap();

    let text = logs.text();
    assert!(
        text.contains("recorded step"),
        "missing recorded-step event:\n{text}"
    );
    assert!(
        text.contains("action=goto") || text.contains("action=Goto"),
        "{text}"
    );
    assert!(
        text.contains("step rejected"),
        "missing reject event:\n{text}"
    );
    assert!(
        text.contains("stable locator"),
        "reject reason not logged:\n{text}"
    );
    assert!(text.contains("tool=\"browser_click\""), "{text}");
    assert!(
        text.contains("session ended"),
        "missing shutdown event:\n{text}"
    );
    assert!(text.contains("agent closed stdin"), "{text}");
}
