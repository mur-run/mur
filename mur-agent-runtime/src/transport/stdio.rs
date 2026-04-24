//! Newline-delimited JSON-RPC 2.0 over stdio.

use crate::protocol::a2a_server::Dispatcher;
use mur_common::JsonRpcRequest;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

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
