//! `mur browser auth`: drive a headed Playwright MCP server privately and
//! export the storage state it captured.

use super::*;

/// The terminal's "logged in": Enter on stdin. Polled only after the login
/// page has opened, so the prompt never appears before the browser.
pub async fn enter_on_stdin() -> Result<()> {
    eprintln!("Complete sign-in in the opened browser, then press Enter here to save the profile.");
    let mut line = String::new();
    let read = tokio::task::spawn_blocking(move || std::io::stdin().read_line(&mut line))
        .await
        .context("wait for login confirmation task")??;
    if read == 0 {
        bail!(
            "login confirmation cancelled: stdin closed — the browser login needs an interactive terminal to wait for Enter"
        );
    }
    Ok(())
}

/// `mur browser auth`'s capture: [`capture_storage_state_on`] with Enter as
/// the "logged in" signal.
pub async fn capture_storage_state(url: &str, browser: &str) -> Result<Vec<u8>> {
    capture_storage_state_on(url, browser, enter_on_stdin()).await
}

/// Spawn a headed Playwright MCP server in an isolated temporary directory,
/// wait for `logged_in`, then return the storage state it exported using
/// `browser_run_code_unsafe`. The unencrypted file remains in the private
/// directory only long enough for this function to read it and
/// `save_profile` to encrypt it locally.
pub async fn capture_storage_state_on(
    url: &str,
    browser: &str,
    logged_in: impl std::future::Future<Output = Result<()>>,
) -> Result<Vec<u8>> {
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
    let result = capture_storage_state_io(child_in, child_out, url, &state_path, logged_in).await;
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
    logged_in: impl std::future::Future<Output = Result<()>>,
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
                Ok(()) => down.capture_storage_state(url, state_path, logged_in).await,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::duplex;
    use tokio::sync::oneshot;

    /// Answers every request with an empty success and reports the tool name
    /// of each `tools/call`.
    async fn fake(
        inp: impl AsyncRead + Unpin,
        mut out: impl AsyncWrite + Unpin,
        seen: mpsc::UnboundedSender<String>,
    ) {
        let mut lines = BufReader::new(inp).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let v: Value = serde_json::from_str(&l).unwrap();
            let Some(id) = v.get("id").cloned() else {
                continue;
            };
            if let Some(name) = v.pointer("/params/name").and_then(Value::as_str) {
                let _ = seen.send(name.to_string());
            }
            let resp = json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [] } });
            out.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
        }
    }

    fn start() -> (
        tokio::task::JoinHandle<Result<()>>,
        mpsc::UnboundedReceiver<String>,
        oneshot::Sender<()>,
    ) {
        let (to_srv, srv_in) = duplex(64 * 1024);
        let (srv_out, from_srv) = duplex(64 * 1024);
        let (seen_tx, seen) = mpsc::unbounded_channel();
        tokio::spawn(fake(srv_in, srv_out, seen_tx));
        let (tx, rx) = oneshot::channel::<()>();
        let dir = tempfile::tempdir().unwrap();
        let run = tokio::spawn(async move {
            let path = dir.path().join("state.json");
            let logged_in = async { rx.await.map_err(|_| anyhow::anyhow!("card closed")) };
            capture_storage_state_io(to_srv, from_srv, "https://x.example", &path, logged_in).await
        });
        (run, seen, tx)
    }

    #[tokio::test]
    async fn nothing_is_exported_before_logged_in() {
        let (run, mut seen, tx) = start();
        assert_eq!(seen.recv().await.as_deref(), Some("browser_navigate"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(seen.try_recv().is_err(), "exported before \"logged in\"");
        tx.send(()).unwrap();
        assert_eq!(
            seen.recv().await.as_deref(),
            Some("browser_run_code_unsafe")
        );
        run.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_closed_card_cancels_without_exporting() {
        let (run, mut seen, tx) = start();
        assert_eq!(seen.recv().await.as_deref(), Some("browser_navigate"));
        drop(tx);
        let err = run.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("card closed"), "{err:#}");
        assert!(seen.try_recv().is_err(), "exported after the card closed");
    }
}
