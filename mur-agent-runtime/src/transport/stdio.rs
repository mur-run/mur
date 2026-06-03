//! Newline-delimited JSON-RPC 2.0 over stdio.

use crate::protocol::a2a_server::Dispatcher;
use mur_common::JsonRpcRequest;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Maximum bytes accepted for a single JSON-RPC line. Aligned with
/// `transport::noise::MAX_FRAME_BYTES` (16 MiB). Oversized lines are dropped
/// rather than dispatched. NOTE: this caps *processing*, not allocation —
/// `read_line` still buffers the full line before returning, so a sender that
/// streams without a newline can still grow the buffer until EOF. Acceptable
/// here because stdio is fed by the controlling parent process (not a remote
/// peer); the network-facing TCP/noise transport is hard-bounded at read time.
/// A fully allocation-bounded reader (LinesCodec/fill_buf) is fast-follow.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

pub async fn serve_stdio<R, W>(
    dispatcher: Arc<Dispatcher>,
    reader: R,
    writer: W,
    mut notifications: mpsc::Receiver<Value>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let w_notif = writer.clone();
    tokio::spawn(async move {
        while let Some(notif) = notifications.recv().await {
            let line = format!("{notif}\n");
            let mut w = w_notif.lock().await;
            let _ = w.write_all(line.as_bytes()).await;
            let _ = w.flush().await;
        }
    });

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        // Guard against a line that grew beyond the cap. This can happen when
        // a sender streams bytes without ever emitting a newline — read_line
        // accumulates until EOF. Discard and keep going rather than processing
        // an arbitrarily large frame.
        if line.len() > MAX_LINE_BYTES {
            tracing::warn!(
                len = line.len(),
                limit_bytes = MAX_LINE_BYTES,
                "stdio: incoming line exceeded cap — discarding"
            );
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(_) => continue, // silently drop malformed; dispatcher also guards
        };
        let resp = match dispatcher.dispatch(req).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let out = match serde_json::to_string(&resp) {
            Ok(s) => format!("{s}\n"),
            Err(_) => continue,
        };
        let mut w = writer.lock().await;
        w.write_all(out.as_bytes()).await?;
        w.flush().await?;
    }
    Ok(())
}
