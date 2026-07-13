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
        // Atomic check+reserve under one lock: the scan and the watcher can
        // both spawn for the same record (FSEvents replays creates from just
        // before the watch started). The old check-then-connect-then-insert
        // left a window where both tasks connected and the later insert
        // overwrote the earlier sender — closing the earlier task's channel,
        // dropping its live stream (peer sees BrokenPipe), and its cleanup
        // then removed the winner's entry too.
        let (out_tx, mut out_rx) = mpsc::channel::<HubFrame>(16);
        {
            let mut guard = senders.lock().unwrap();
            if guard.contains_key(&pid) {
                return; // scan/watcher overlap
            }
            guard.insert(pid, out_tx.clone());
        }
        let Ok(stream) = UnixStream::connect(&sess.sock).await else {
            // Socket gone but record present: crashed murmur. Reap.
            senders.lock().unwrap().remove(&pid);
            tracing::debug!("panel_bridge: reaping dead session pid={pid}");
            let _ = std::fs::remove_file(&json_path);
            let _ = std::fs::remove_file(&sess.sock);
            return;
        };
        tracing::info!("panel_bridge: connected to murmur session pid={pid}");
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
        // Remove only OUR entry: with atomic reservation no duplicate should
        // exist, but guard against removing a newer session's sender anyway.
        {
            let mut guard = senders.lock().unwrap();
            if guard.get(&pid).is_some_and(|s| s.same_channel(&out_tx)) {
                guard.remove(&pid);
            }
        }
        let _ = tx.send(PanelEvent::SessionDown { pid }).await;
    });
}
