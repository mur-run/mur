//! Newline-delimited JSON-RPC 2.0 over stdio.

use crate::protocol::a2a_server::{Dispatcher, RequestContext};
use futures::StreamExt;
use mur_common::JsonRpcRequest;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodec};

/// Maximum bytes accepted for a single JSON-RPC line. Aligned with
/// `transport::noise::MAX_FRAME_BYTES` (16 MiB). `LinesCodec` enforces this
/// at read time — the buffer never grows beyond this limit before a line is
/// returned or the codec errors out, giving true allocation-bounded reads.
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

    let codec = LinesCodec::new_with_max_length(MAX_LINE_BYTES);
    let mut framed = FramedRead::new(reader, codec);

    while let Some(result) = framed.next().await {
        let line = match result {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "stdio: codec error — discarding frame");
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(_) => continue, // silently drop malformed; dispatcher also guards
        };
        // stdio is a single peer; notifications already fan to this one stdout
        // via the baked notifier, so no per-connection routing is needed.
        let resp = match dispatcher.dispatch(req, &RequestContext::none()).await {
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
