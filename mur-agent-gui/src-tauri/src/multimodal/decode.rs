//! GUI-side client that spawns the `mur-agent-decoder` subprocess.
//!
//! Sends one length-prefixed `DecodeRequest` frame on stdin, reads one
//! `DecodeResponse` frame from stdout, then waits for the child to
//! exit. A `tokio::time::timeout` wraps the read so a hung decoder
//! doesn't dangle the GUI; on timeout the child is dropped (which on
//! `tokio::process::Child` triggers `kill_on_drop` semantics — but we
//! also call `start_kill` explicitly for clarity).

use anyhow::{Context, Result};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::decoder_protocol::{DecodeRequest, DecodeResponse, read_frame};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct DecoderClient {
    binary: std::path::PathBuf,
    timeout: Duration,
}

impl DecoderClient {
    pub fn new() -> Self {
        Self {
            binary: locate_decoder_binary(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            ..Self::new()
        }
    }

    pub async fn decode_image(&self, bytes: Vec<u8>, mime_hint: &str) -> Result<DecodeResponse> {
        self.invoke(DecodeRequest::Image {
            bytes,
            mime_hint: mime_hint.into(),
        })
        .await
    }

    pub async fn decode_pdf(&self, bytes: Vec<u8>) -> Result<DecodeResponse> {
        self.invoke(DecodeRequest::Pdf { bytes }).await
    }

    async fn invoke(&self, req: DecodeRequest) -> Result<DecodeResponse> {
        let mut child = Command::new(&self.binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {}", self.binary.display()))?;

        let req_bytes = serde_json::to_vec(&req)?;
        if let Some(stdin) = child.stdin.as_mut() {
            // Length-prefix frame, then close stdin to signal EOF.
            let len = (req_bytes.len() as u32).to_be_bytes();
            stdin.write_all(&len).await?;
            stdin.write_all(&req_bytes).await?;
            stdin.flush().await?;
        }
        drop(child.stdin.take());

        let mut stdout = child.stdout.take().context("missing stdout")?;
        let timeout = self.timeout;
        let resp = match tokio::time::timeout(timeout, async move {
            // Pull the entire stdout into memory; child exits after one
            // frame so this is bounded by the response size (capped at
            // 64 MiB by the read_frame helper).
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).await?;
            let mut cursor = std::io::Cursor::new(buf);
            let frame = read_frame(&mut cursor)?;
            Ok::<DecodeResponse, anyhow::Error>(serde_json::from_slice(&frame)?)
        })
        .await
        {
            Ok(inner) => inner?,
            Err(_) => {
                // Timed out: kill the child explicitly. `kill_on_drop`
                // would also handle this, but being explicit makes the
                // failure mode obvious.
                let _ = child.start_kill();
                let _ = child.wait().await;
                anyhow::bail!("decoder timed out after {:?}", timeout);
            }
        };

        let _ = child.wait().await;
        Ok(resp)
    }
}

impl Default for DecoderClient {
    fn default() -> Self {
        Self::new()
    }
}

fn locate_decoder_binary() -> std::path::PathBuf {
    // Test override + cargo bin discovery first.
    if let Ok(p) = std::env::var("MUR_AGENT_DECODER_BIN") {
        return std::path::PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_mur-agent-decoder") {
        return std::path::PathBuf::from(p);
    }
    // In a Tauri bundle the decoder ships next to the main binary.
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .map(|d| {
            d.join(if cfg!(windows) {
                "mur-agent-decoder.exe"
            } else {
                "mur-agent-decoder"
            })
        })
        .unwrap_or_else(|| std::path::PathBuf::from("mur-agent-decoder"))
}
