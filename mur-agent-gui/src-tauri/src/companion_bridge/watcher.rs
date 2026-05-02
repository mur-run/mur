//! Filesystem watcher that turns new `.md` writes under the agent's
//! companion inbox directory into BridgeEvent records on a tokio mpsc.
//!
//! Why notify (not polling): we need ≤ 1 s end-to-end latency from
//! outbox tick to GUI. notify uses kqueue (macOS), inotify (Linux),
//! ReadDirectoryChangesW (Windows) — all sub-100 ms. Polling at 1 s
//! would push us over budget once the React render lands on top.
//!
//! Why we ignore non-Create events: outbox always uses
//! O_CREAT | O_EXCL (see runtime's inbox.rs); a Modify on an inbox
//! file is the user pressing 👍/👎/🚫 (the response line is rewritten
//! in place) and is NOT a new message. Filtering on Create avoids a
//! double-emit.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::Sender;

use super::event::{BridgeEvent, parse_inbox_md};

pub struct InboxWatcher {
    /// Hold the watcher alive until the bridge is dropped.
    _inner: RecommendedWatcher,
}

impl InboxWatcher {
    pub fn start(inbox_dir: PathBuf, tx: Sender<BridgeEvent>) -> Result<Self> {
        std::fs::create_dir_all(&inbox_dir)
            .with_context(|| format!("create {}", inbox_dir.display()))?;
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else { return };
                if !matches!(event.kind, EventKind::Create(_)) {
                    return;
                }
                for path in event.paths {
                    if path.extension().and_then(|s| s.to_str()) != Some("md") {
                        continue;
                    }
                    forward_md(&path, &tx);
                }
            },
            Config::default(),
        )
        .context("notify::watcher::new")?;
        watcher
            .watch(&inbox_dir, RecursiveMode::NonRecursive)
            .with_context(|| format!("watch {}", inbox_dir.display()))?;
        Ok(Self { _inner: watcher })
    }
}

fn forward_md(path: &Path, tx: &Sender<BridgeEvent>) {
    match parse_inbox_md(path) {
        Ok(ev) => {
            // try_send drops if the receiver buffer is full (>=8). The UI
            // can always re-scan via scan_pending if it falls behind.
            if let Err(e) = tx.try_send(ev) {
                tracing::warn!(
                    "companion_bridge: dropped event for {}: {e}",
                    path.display()
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                "companion_bridge: skipping malformed write at {}: {e:#}",
                path.display()
            );
        }
    }
}
