//! Production transport: line-delimited JSON-RPC over a spawned
//! Playwright MCP server's stdio, plus the `replay_live` entry point.

use super::{ReplayReport, ToolCaller, check_navigation, replay_with};
use crate::recorder::Run;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};

/// Sequential line-delimited JSON-RPC client over a server's stdio. Replay
/// issues one request at a time, so no id multiplexing is needed.
pub struct StdioCaller<W, R> {
    writer: W,
    lines: Lines<BufReader<R>>,
    next_id: u64,
}

impl<W, R> StdioCaller<W, R>
where
    W: AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
{
    /// Perform the MCP `initialize` handshake.
    pub async fn connect(writer: W, reader: R) -> Result<Self> {
        let mut caller = Self {
            writer,
            lines: BufReader::new(reader).lines(),
            next_id: 1,
        };
        caller
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "mur browser replay", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await
            .context("initialize Playwright MCP session")?;
        caller
            .send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))
            .await?;
        Ok(caller)
    }

    async fn send(&mut self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .context("write to MCP server")?;
        self.writer.flush().await.context("flush MCP server stdin")
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = format!("mur-replay-{}", self.next_id);
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        while let Some(line) = self.lines.next_line().await? {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue; // stray non-JSON output
            };
            if message.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                continue; // notifications or unrelated traffic
            }
            if let Some(error) = message.get("error") {
                bail!("{method} failed: {error}");
            }
            return message
                .get("result")
                .cloned()
                .with_context(|| format!("{method} returned neither result nor error"));
        }
        bail!("MCP server exited before answering {method}")
    }
}

impl<W, R> ToolCaller for StdioCaller<W, R>
where
    W: AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
{
    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }
}

/// Launch arguments for the headless Playwright MCP server used by replay.
pub(super) fn live_args(storage_state: Option<&std::path::Path>) -> Vec<String> {
    // @playwright/mcp defaults to branded Google Chrome, which is often not
    // installed; `--browser=chromium` selects Playwright's own build.
    let mut args = vec![
        "--headless".to_owned(),
        "--isolated".to_owned(),
        "--browser=chromium".to_owned(),
        // browser_verify_* (assert steps) are opt-in in @playwright/mcp.
        "--caps=testing".to_owned(),
    ];
    if let Some(path) = storage_state {
        args.push(format!("--storage-state={}", path.display()));
    }
    // The package launches full Chrome for Testing even headless; point it
    // at the headless shell `mur browser setup` installs, when present.
    args.extend(crate::chromium::headless_exe_args(
        crate::chromium::system_browsers_dir().as_deref(),
    ));
    args
}

/// Spawn a headless, isolated Playwright MCP server and replay `run` on it.
/// `storage_state` is a decrypted profile state file the caller owns and
/// deletes afterwards.
pub async fn replay_live(
    run: &Run,
    allow: &[String],
    storage_state: Option<&std::path::Path>,
) -> Result<ReplayReport> {
    check_navigation(run, allow)?;
    let args = live_args(storage_state);
    let mut child = crate::proxy::playwright_command(&args)
        .spawn()
        .context("spawn Playwright MCP server (is `npx` on PATH and in the spawn allowlist?)")?;
    let stdin = child.stdin.take().context("child stdin")?;
    let stdout = child.stdout.take().context("child stdout")?;
    let result = async {
        let mut caller = StdioCaller::connect(stdin, stdout).await?;
        replay_with(run, allow, &mut caller).await
    }
    .await;
    let _ = child.kill().await;
    result
}
