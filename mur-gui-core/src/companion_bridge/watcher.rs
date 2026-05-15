//! Filesystem watcher — turns new `.md` writes under an agent's
//! companion inbox directory into `BridgeEvent` records on a tokio mpsc.
//!
//! Uses `notify` (kqueue / inotify / ReadDirectoryChangesW) for
//! sub-100 ms latency. Only `Create` events are forwarded; `Modify`
//! events are the user's ack rewrite and are intentionally ignored.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::Sender;

use super::event::{BridgeEvent, parse_inbox_md};

pub struct InboxWatcher {
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
