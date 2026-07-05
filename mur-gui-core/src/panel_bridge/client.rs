//! One task per murmur session: connect, pump frames, report down.

use std::path::PathBuf;

use mur_common::panel::{HubFrame, PanelFrame, PanelSession, decode_line};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use super::{PanelEvent, Senders};

pub(crate) fn spawn(
    rt: tokio::runtime::Handle,
    json_path: PathBuf,
    senders: Senders,
    tx: mpsc::Sender<PanelEvent>,
) {
    rt.spawn(async move {
        let Ok(bytes) = std::fs::read(&json_path) else {
            return;
        };
        let Ok(sess) = serde_json::from_slice::<PanelSession>(&bytes) else {
            tracing::warn!(
                "panel_bridge: malformed session record {}",
                json_path.display()
            );
            return;
        };
        let pid = sess.pid;
        if senders.lock().unwrap().contains_key(&pid) {
            return; // scan/watcher overlap
        }
        let Ok(stream) = UnixStream::connect(&sess.sock).await else {
            // Socket gone but record present: crashed murmur. Reap.
            let _ = std::fs::remove_file(&json_path);
            let _ = std::fs::remove_file(&sess.sock);
            return;
        };
        let (out_tx, mut out_rx) = mpsc::channel::<HubFrame>(16);
        senders.lock().unwrap().insert(pid, out_tx);
        let (r, mut w) = stream.into_split();
        let mut lines = BufReader::new(r).lines();

        loop {
            tokio::select! {
                maybe = lines.next_line() => match maybe {
                    Ok(Some(line)) => {
                        if let Some(f) = decode_line::<PanelFrame>(&line) {
                            let _ = tx.send(PanelEvent::Frame { pid, frame: f }).await;
                        } // unknown frames: skipped (forward compat)
                    }
                    _ => break, // EOF/error: session down
                },
                maybe = out_rx.recv() => match maybe {
                    Some(f) => {
                        let Ok(mut buf) = serde_json::to_vec(&f) else { break };
                        buf.push(b'\n');
                        if w.write_all(&buf).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
            }
        }
        senders.lock().unwrap().remove(&pid);
        let _ = tx.send(PanelEvent::SessionDown { pid }).await;
    });
}
