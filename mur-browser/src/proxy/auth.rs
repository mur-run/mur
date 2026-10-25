//! `mur browser auth`: drive a headed Playwright MCP server privately and
//! export the storage state it captured.

use super::*;

/// Spawn a headed Playwright MCP server in an isolated temporary directory,
/// then return the storage state it exported using `browser_run_code_unsafe`.
/// The unencrypted file remains in the private directory only long enough for
/// this function to read it and `save_profile` to encrypt it locally.
pub async fn capture_storage_state(url: &str, browser: &str) -> Result<Vec<u8>> {
    let output = private_output_dir()?;
    let output_arg = format!("--output-dir={}", output.display());
    let browser_arg = format!("--browser={browser}");
    // @playwright/mcp runs headed by default; its CLI has no `--headed` flag.
    let mut args = vec!["--isolated".into(), browser_arg, output_arg];
    args.extend(crate::chromium::system_headed_extra_args(browser));
    let mut child = match playwright_command(&args).and_then(|mut cmd| {
        cmd.spawn().context(
            "spawn headed Playwright MCP server (is `node` on PATH and in the spawn allowlist?)",
        )
    }) {
        Ok(child) => child,
        Err(error) => {
            // No state exists yet, but do not leave private scratch directories
            // behind when the server cannot start.
            let _ = std::fs::remove_dir_all(&output);
            return Err(error);
        }
    };
    let child_in = match child.stdin.take().context("child stdin") {
        Ok(stdin) => stdin,
        Err(error) => {
            let _ = child.kill().await;
            let _ = std::fs::remove_dir_all(&output);
            return Err(error);
        }
    };
    let child_out = match child.stdout.take().context("child stdout") {
        Ok(stdout) => stdout,
        Err(error) => {
            let _ = child.kill().await;
            let _ = std::fs::remove_dir_all(&output);
            return Err(error);
        }
    };
    let state_path = output.join("storage-state.json");
    let result = capture_storage_state_io(child_in, child_out, url, &state_path).await;
    let _ = child.kill().await;
    let state = match result {
        Ok(()) => std::fs::read(&state_path).with_context(|| {
            format!(
                "read captured browser storage state {}",
                state_path.display()
            )
        }),
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_dir_all(&output);
    state
}

pub(super) fn private_output_dir() -> Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("mur-browser-auth-{}", uuid::Uuid::new_v4()));
    // The Playwright tool writes unencrypted session cookies here. Do not rely
    // on the caller's umask: only this user may traverse the directory.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .with_context(|| {
                format!("create private browser output directory {}", path.display())
            })?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(&path).with_context(|| {
            format!("create private browser output directory {}", path.display())
        })?;
    }
    Ok(path)
}

async fn capture_storage_state_io<SI, SO>(
    server_in: SI,
    server_out: SO,
    url: &str,
    state_path: &std::path::Path,
) -> Result<()>
where
    SI: AsyncWrite + Unpin + Send + 'static,
    SO: AsyncRead + Unpin + Send + 'static,
{
    let (to_server, mut to_server_rx) = mpsc::unbounded_channel::<String>();
    let down = Downstream::new(to_server);
    let server_task = tokio::spawn(async move {
        let mut server_in = server_in;
        while let Some(line) = to_server_rx.recv().await {
            server_in.write_all(line.as_bytes()).await?;
            server_in.write_all(b"\n").await?;
            server_in.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    });
    let downstream = down.clone();
    let response_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_out).lines();
        while let Some(line) = lines.next_line().await? {
            let value: Value = serde_json::from_str(&line)?;
            if let Some(id) = value.get("id")
                && downstream.is_internal_id(id)
                && let Some(sender) = downstream
                    .pending
                    .lock()
                    .await
                    .remove(id.as_str().unwrap_or(""))
            {
                let _ = sender.send(value);
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let initialized = down
        .call(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "mur browser auth",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )
        .await
        .context("initialize headed Playwright MCP session");
    let result = match initialized {
        Ok(response) => match ensure_response_succeeded("initialize", &response) {
            Ok(_) => match down
                .notify("notifications/initialized", json!({}))
                .context("confirm headed Playwright MCP initialization")
            {
                Ok(()) => down.capture_storage_state(url, state_path).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    drop(down);
    // The response reader owns a `Downstream` clone, and therefore the last
    // sender to the server writer. Stop it before awaiting the writer or this
    // private session would wait forever for its own channel to close.
    response_task.abort();
    let _ = response_task.await;
    let _ = server_task.await;
    result
}
