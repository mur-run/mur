//! Hub-side bridge to live murmur TUI sessions. Watches
//! `murmur_run_dir` for `<pid>.json` + `<pid>.sock` records, connects a
//! client per session, republishes frames as [`PanelEvent`]s; sends
//! [`PanelBridge::insert`] back (insert-only by protocol design).

mod client;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use mur_common::panel::{HubFrame, PanelFrame, murmur_run_dir};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum PanelEvent {
    /// Any frame from a session, including the initial `Hello`.
    Frame {
        pid: u32,
        frame: PanelFrame,
    },
    SessionDown {
        pid: u32,
    },
}

pub(crate) type Senders = Arc<Mutex<HashMap<u32, mpsc::Sender<HubFrame>>>>;

pub struct PanelBridge {
    senders: Senders,
    _watcher: RecommendedWatcher,
}

impl PanelBridge {
    /// Scan for existing sessions and watch for new ones. Must be called
    /// from within a tokio runtime (spawns per-session client tasks).
    pub fn start(mur_home: PathBuf, tx: mpsc::Sender<PanelEvent>) -> Result<Self> {
        let dir = murmur_run_dir(&mur_home);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let senders: Senders = Arc::new(Mutex::new(HashMap::new()));
        let rt = tokio::runtime::Handle::current();

        // Watcher first, then scan: a session appearing between the two is
        // caught by the watcher; one appearing before the scan is caught by
        // the scan; the contains_key guard in client.rs dedups an overlap.
        let (w_senders, w_tx, w_rt) = (senders.clone(), tx.clone(), rt.clone());
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else { return };
                if !matches!(event.kind, EventKind::Create(_)) {
                    return;
                }
                for path in event.paths {
                    if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        client::spawn(w_rt.clone(), path, w_senders.clone(), w_tx.clone());
                    }
                }
            },
            notify::Config::default(),
        )?;
        watcher
            .watch(&dir, RecursiveMode::NonRecursive)
            .with_context(|| format!("watch {}", dir.display()))?;

        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                client::spawn(rt.clone(), path, senders.clone(), tx.clone());
            }
        }

        Ok(Self {
            senders,
            _watcher: watcher,
        })
    }

    /// Insert-only by design: `text` fills murmur's input box, never runs.
    /// Returns false when the session is gone (or its buffer is full).
    pub fn insert(&self, pid: u32, text: String) -> bool {
        self.senders
            .lock()
            .unwrap()
            .get(&pid)
            .map(|s| s.try_send(HubFrame::Insert { text }).is_ok())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::panel::{
        HubFrame, PANEL_PROTO_VERSION, PanelFrame, PanelSession, murmur_run_dir,
    };
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    /// Murmur stand-in: bind the socket and write the session record.
    async fn fake_murmur(home: &std::path::Path, pid: u32) -> (UnixListener, std::path::PathBuf) {
        let dir = murmur_run_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join(format!("{pid}.sock"));
        let listener = UnixListener::bind(&sock).unwrap();
        let session = PanelSession {
            pid,
            agent: "fake".into(),
            cwd: "/tmp".into(),
            sock: sock.to_string_lossy().into_owned(),
            terminal_program: None,
            proto_version: PANEL_PROTO_VERSION,
        };
        let json = dir.join(format!("{pid}.json"));
        std::fs::write(&json, serde_json::to_vec(&session).unwrap()).unwrap();
        (listener, json)
    }

    #[tokio::test]
    async fn discovers_pumps_and_reports_down() {
        let tmp = tempfile::tempdir().unwrap();
        let (listener, _json) = fake_murmur(tmp.path(), 7001).await;
        let (tx, mut rx) = mpsc::channel(16);
        let bridge = PanelBridge::start(tmp.path().to_path_buf(), tx).unwrap();

        // Bridge connects; fake murmur sends a frame in lieu of Hello.
        let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (r, mut w) = stream.into_split();
        let frame_line = serde_json::to_string(&PanelFrame::Panel {
            focus: mur_common::panel::PanelTab::Information,
        })
        .unwrap();
        w.write_all(format!("{frame_line}\n").as_bytes())
            .await
            .unwrap();

        let ev = timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let PanelEvent::Frame {
            pid: 7001,
            frame: PanelFrame::Panel { .. },
        } = ev
        else {
            panic!("expected Panel frame event, got {ev:?}");
        };

        // insert() reaches the fake server as a HubFrame line.
        assert!(bridge.insert(7001, "/help".into()));
        let mut lines = BufReader::new(r).lines();
        let line = timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            mur_common::panel::decode_line::<HubFrame>(&line),
            Some(HubFrame::Insert { text }) if text == "/help"
        ));

        // Server close → SessionDown, insert() now false.
        drop(w);
        drop(lines);
        drop(listener);
        let ev = timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ev, PanelEvent::SessionDown { pid: 7001 }));
        assert!(!bridge.insert(7001, "x".into()));
    }

    #[tokio::test]
    async fn stale_session_json_is_reaped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = murmur_run_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        let json = dir.join("7002.json");
        let session = PanelSession {
            pid: 7002,
            agent: "dead".into(),
            cwd: "/tmp".into(),
            sock: dir.join("7002.sock").to_string_lossy().into_owned(),
            terminal_program: None,
            proto_version: PANEL_PROTO_VERSION,
        };
        std::fs::write(&json, serde_json::to_vec(&session).unwrap()).unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let _bridge = PanelBridge::start(tmp.path().to_path_buf(), tx).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!json.exists());
    }
}
