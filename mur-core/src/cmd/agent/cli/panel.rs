//! Panel bridge server: one Unix socket per murmur session that MUR Hub
//! connects to. murmur pushes `PanelFrame`s; Hub pushes `HubFrame`s
//! (insert-only). The session record + socket live under
//! `murmur_run_dir(home)` and vanish when the TUI exits — the vanished
//! socket is how the Hub learns the session ended.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mur_common::panel::{
    HubFrame, PANEL_PROTO_VERSION, PanelFrame, PanelSession, decode_line, murmur_run_dir,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

const CHANNEL_CAP: usize = 64;

pub struct PanelHandle {
    out_tx: mpsc::Sender<PanelFrame>,
    /// Keeps the returned receiver open even when the server task is gone —
    /// a closed channel would complete instantly on every `select!` poll and
    /// spin the TUI event loop.
    _keepalive: mpsc::Sender<HubFrame>,
    json_path: PathBuf,
    sock_path: PathBuf,
}

impl PanelHandle {
    /// Fire-and-forget: frames beyond `CHANNEL_CAP` while no Hub is
    /// connected are dropped.
    pub fn send(&self, frame: PanelFrame) {
        let _ = self.out_tx.try_send(frame);
    }
}

impl Drop for PanelHandle {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.json_path);
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

/// Start the per-session panel server. Never fails the TUI: on any setup
/// error the server is disabled and murmur runs without a panel. Must be
/// called from within a tokio runtime.
pub fn start(home: &Path, agent: &str, cwd: &Path) -> (mpsc::Receiver<HubFrame>, PanelHandle) {
    let pid = std::process::id();
    let dir = murmur_run_dir(home);
    let json_path = dir.join(format!("{pid}.json"));
    let sock_path = dir.join(format!("{pid}.sock"));
    let (in_tx, rx) = mpsc::channel(CHANNEL_CAP);
    let (out_tx, out_rx) = mpsc::channel(CHANNEL_CAP);
    let handle = PanelHandle {
        out_tx,
        _keepalive: in_tx.clone(),
        json_path: json_path.clone(),
        sock_path: sock_path.clone(),
    };
    let session = PanelSession {
        pid,
        agent: agent.to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        sock: sock_path.to_string_lossy().into_owned(),
        terminal_program: std::env::var("TERM_PROGRAM").ok(),
        proto_version: PANEL_PROTO_VERSION,
    };
    if let Err(e) = serve(&dir, &json_path, &sock_path, session, in_tx, out_rx) {
        tracing::warn!("panel: server disabled: {e:#}");
    }
    (rx, handle)
}

fn serve(
    dir: &Path,
    json_path: &Path,
    sock_path: &Path,
    session: PanelSession,
    in_tx: mpsc::Sender<HubFrame>,
    out_rx: mpsc::Receiver<PanelFrame>,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    // A crashed previous run with the same (recycled) pid may have left files.
    let _ = std::fs::remove_file(sock_path);
    let listener = UnixListener::bind(sock_path).context("bind panel socket")?;
    std::fs::set_permissions(sock_path, std::fs::Permissions::from_mode(0o600))?;
    let json = serde_json::to_vec(&session)?;
    std::fs::write(json_path, json).context("write session record")?;
    std::fs::set_permissions(json_path, std::fs::Permissions::from_mode(0o600))?;
    tokio::spawn(accept_loop(listener, session, in_tx, out_rx));
    Ok(())
}

/// One Hub client at a time; a new connection is served after the previous
/// one disconnects.
async fn accept_loop(
    listener: UnixListener,
    session: PanelSession,
    in_tx: mpsc::Sender<HubFrame>,
    mut out_rx: mpsc::Receiver<PanelFrame>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        if !pump(stream, &session, &in_tx, &mut out_rx).await {
            // TUI side dropped its sender: shut down for good.
            return;
        }
    }
}

/// Returns true when the client went away (keep accepting), false when the
/// TUI's outgoing channel closed (shut down).
async fn pump(
    stream: UnixStream,
    session: &PanelSession,
    in_tx: &mpsc::Sender<HubFrame>,
    out_rx: &mut mpsc::Receiver<PanelFrame>,
) -> bool {
    let (r, mut w) = stream.into_split();
    let hello = PanelFrame::Hello {
        session: session.clone(),
    };
    if write_line(&mut w, &hello).await.is_err() {
        return true;
    }
    let mut lines = BufReader::new(r).lines();
    loop {
        tokio::select! {
            maybe = lines.next_line() => match maybe {
                Ok(Some(line)) => {
                    if let Some(f) = decode_line::<HubFrame>(&line) {
                        let _ = in_tx.send(f).await;
                    } // unknown frames: skipped (forward compat)
                }
                _ => return true, // client EOF/error → back to accept
            },
            maybe = out_rx.recv() => match maybe {
                Some(f) => {
                    if write_line(&mut w, &f).await.is_err() {
                        return true;
                    }
                }
                None => return false,
            },
        }
    }
}

const PANEL_HINT: &str =
    "usage: /panel [information|activities|preview|notifications] · /panel preview <path|url>";

/// `/panel [tab] [target]` — fire-and-forget; opens/focuses the Hub Panel
/// window on the given tab.
pub fn handle_panel_command(app: &mut super::app::App, args: &[String]) {
    use mur_common::panel::{PanelTab, PreviewKind};
    let frame = match args.first().map(String::as_str) {
        Some("information" | "info") => PanelFrame::Panel {
            focus: PanelTab::Information,
        },
        Some("activities") => PanelFrame::Panel {
            focus: PanelTab::Activities,
        },
        Some("notifications") => PanelFrame::Panel {
            focus: PanelTab::Notifications,
        },
        Some("preview") => match args.get(1) {
            Some(t) if t.starts_with("http://") || t.starts_with("https://") => {
                PanelFrame::Preview {
                    kind: PreviewKind::Url,
                    target: t.clone(),
                }
            }
            Some(t) => PanelFrame::Preview {
                kind: PreviewKind::File,
                target: absolutize(app.cwd.as_deref().unwrap_or(Path::new(".")), t),
            },
            None => PanelFrame::Panel {
                focus: PanelTab::Preview,
            },
        },
        None => PanelFrame::Panel {
            focus: PanelTab::Information,
        },
        Some(other) => {
            app.push_system(format!("unknown panel tab: {other} — {PANEL_HINT}"));
            return;
        }
    };
    ensure_hub_running(app);
    if let Some(panel) = &app.panel {
        panel.send(frame);
    }
}

/// Relative preview targets resolve against the session cwd.
fn absolutize(cwd: &Path, target: &str) -> String {
    let p = Path::new(target);
    if p.is_absolute() {
        target.to_string()
    } else {
        cwd.join(p).to_string_lossy().into_owned()
    }
}

#[cfg(target_os = "macos")]
fn ensure_hub_running(app: &mut super::app::App) {
    // -g: don't steal focus from the terminal. No-op when already running.
    let ok = std::process::Command::new("open")
        .args(["-g", "-a", "MUR Hub"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        app.push_system("panel: MUR Hub app not found — install the Hub to use /panel");
    }
}

#[cfg(not(target_os = "macos"))]
fn ensure_hub_running(_app: &mut super::app::App) {}

async fn write_line(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    f: &PanelFrame,
) -> std::io::Result<()> {
    let mut buf = serde_json::to_vec(f).map_err(std::io::Error::other)?;
    buf.push(b'\n');
    w.write_all(&buf).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::panel::{PanelFrame, PanelSession, murmur_run_dir};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    #[test]
    fn absolutize_paths() {
        assert_eq!(
            absolutize(Path::new("/repo"), "out/x.html"),
            "/repo/out/x.html"
        );
        assert_eq!(absolutize(Path::new("/repo"), "/abs/x.html"), "/abs/x.html");
    }

    #[tokio::test]
    async fn hello_insert_bye_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut rx, handle) = start(tmp.path(), "mur", std::path::Path::new("/tmp"));
        let pid = std::process::id();
        let dir = murmur_run_dir(tmp.path());
        let json = dir.join(format!("{pid}.json"));
        let sock = dir.join(format!("{pid}.sock"));
        assert!(json.exists(), "session record written");

        let stream = UnixStream::connect(&sock).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut lines = BufReader::new(r).lines();

        // Hello arrives first, carries the session.
        let line = lines.next_line().await.unwrap().unwrap();
        let PanelFrame::Hello { session } =
            mur_common::panel::decode_line::<PanelFrame>(&line).unwrap()
        else {
            panic!("expected hello, got {line}");
        };
        assert_eq!(session.agent, "mur");
        let parsed: PanelSession = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
        assert_eq!(parsed.pid, pid);

        // Hub → murmur insert.
        w.write_all(b"{\"type\":\"insert\",\"text\":\"/help\"}\n")
            .await
            .unwrap();
        let mur_common::panel::HubFrame::Insert { text } = rx.recv().await.unwrap();
        assert_eq!(text, "/help");

        // murmur → Hub frame (buffered try_send path).
        handle.send(PanelFrame::Bye);
        let line = lines.next_line().await.unwrap().unwrap();
        assert!(line.contains("\"type\":\"bye\""));

        // Drop removes both files.
        drop(handle);
        assert!(!json.exists() && !sock.exists());
    }
}
