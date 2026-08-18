use std::path::Path;

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// Watch `<mur_home>/channels/` recursively. For every filesystem event whose
/// path is inside a channel dir, invoke `on_change(channel_id)`. Returns the
/// watcher; the caller must keep it alive for the watch to persist.
pub fn watch_channels(
    mur_home: &Path,
    on_change: impl Fn(String) + Send + 'static,
) -> Result<RecommendedWatcher> {
    let root = mur_home.join("channels");
    std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
    // Canonicalize so event paths match the prefix regardless of backend:
    // macOS FSEvents reports canonical paths (/var/folders/… → /private/var/…),
    // while Linux inotify reports paths as-registered. Registering the watch AND
    // stripping with the SAME canonical path keeps both backends consistent — a
    // symlinked mur_home otherwise breaks the prefix match on Linux.
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", root.display()))?;
    let strip_root = root_canon.clone();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        for path in event.paths {
            if let Ok(rel) = path.strip_prefix(&strip_root)
                && let Some(first) = rel.components().next()
                && let Some(id) = first.as_os_str().to_str()
            {
                on_change(id.to_string());
            }
        }
    })
    .context("create watcher")?;
    watcher
        .watch(&root_canon, RecursiveMode::Recursive)
        .context("start watch")?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    use crate::ChannelStore;
    use chrono::Utc;
    use mur_common::channel::{Channel, ChannelActor, ChannelState, EventKind, Goal};

    #[test]
    fn append_fires_callback_with_channel_id() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        let now = Utc::now();
        store
            .create(&Channel {
                v: 1,
                id: "c9".into(),
                title: "t".into(),
                goal: Goal::default(),
                state: ChannelState::Working,
                purpose: None,
                owner: ChannelActor::Human { name: "me".into() },
                participants: vec![],
                created_at: now,
                updated_at: now,
            })
            .unwrap();

        let (tx, rx) = mpsc::channel::<String>();
        let _watcher = watch_channels(tmp.path(), move |id| {
            let _ = tx.send(id);
        })
        .unwrap();

        // Give the watcher a moment to arm, then append.
        std::thread::sleep(Duration::from_millis(500));
        store
            .append_event(
                "c9",
                ChannelActor::Human { name: "me".into() },
                EventKind::Message,
                serde_json::json!({"text":"hi"}),
                None,
                None,
                None,
            )
            .unwrap();

        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("callback fired");
        assert_eq!(got, "c9");
    }
}
