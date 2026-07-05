# Murmur Panel P1 — End-to-End Skeleton Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The P1 skeleton of the murmur Panel companion window: a murmur session is discoverable by the MUR Hub over a per-session Unix socket, `/panel` opens a floating Hub window snapped beside the terminal, and a click in the Panel inserts text into murmur's input box.

**Architecture:** murmur (TUI, `mur-core`) hosts a JSON-lines Unix-socket server per session and advertises it via `~/.mur/runtime/murmur/<pid>.json`. `mur-gui-core::panel_bridge` watches that directory, connects a client per session, and republishes frames. `mur-hub-gui` opens a `murmur-panel` webview window (four tabs, P1 mostly placeholders), snap-once positioned via a CGWindowList lookup behind a clean `reposition()` seam.

**Tech Stack:** Rust (edition 2024, tokio, serde, notify), Tauri 2 + React (Hub), core-foundation FFI (macOS window bounds).

**Spec:** `docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md`

## Global Constraints

- Branch: `feat/murmur-panel-p1` off `main`. Run `git branch --show-current` before EVERY commit (main advances mid-session).
- Env preamble for every cargo command touching `mur-core`:
  `export ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist`
- Never `cargo build --workspace`. Use `cargo check -p <crate>` / `cargo test -p <crate> <filter>`.
- `mur-hub-gui` is workspace-excluded: check it with `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`, after `mkdir -p mur-hub-gui/ui/dist && touch mur-hub-gui/ui/dist/index.html` (stub; NEVER commit it).
- Single source file ≤ 800 lines. Brand string is uppercase **MUR** in all user-visible text ("MUR Hub", "MUR Panel").
- Panel protocol is insert-only: no frame may trigger execution. Session/socket files live only under `~/.mur/runtime/murmur/` (helper in `mur-common::panel`; no hardcoded paths elsewhere).
- Commit messages end with:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

---

### Task 0: Branch

- [ ] **Step 0.1:**

```bash
cd /Volumes/Firecuda4tb/Projects/mur && git checkout main && git pull && git checkout -b feat/murmur-panel-p1
```

---

### Task 1: Protocol types — `mur-common::panel`

**Files:**
- Create: `mur-common/src/panel.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod panel;` after `pub mod muragent;`)
- Modify: `docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md` (path amendment)

**Interfaces:**
- Produces: `mur_common::panel::{PANEL_PROTO_VERSION: u32, murmur_run_dir(&Path) -> PathBuf, PanelSession, PanelTab, PreviewKind, PanelFrame, HubFrame, decode_line<T>(&str) -> Option<T>}` — exact shapes below. All later tasks consume these verbatim.

- [ ] **Step 1.1: Write the failing test** — create `mur-common/src/panel.rs` with tests only (module not yet in lib.rs, so first add `pub mod panel;` to `mur-common/src/lib.rs` after `pub mod muragent;`):

```rust
//! Wire protocol + on-disk session records for the murmur Panel (companion
//! window). Shared by the murmur TUI (socket server side) and
//! `mur-gui-core::panel_bridge` (Hub client side). One JSON object per line.
//! Design: docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn run_dir_under_runtime() {
        assert_eq!(
            murmur_run_dir(Path::new("/h")),
            Path::new("/h/runtime/murmur")
        );
    }

    #[test]
    fn frames_round_trip() {
        let s = PanelSession {
            pid: 42,
            agent: "mur".into(),
            cwd: "/tmp".into(),
            sock: "/h/runtime/murmur/42.sock".into(),
            terminal_program: Some("iTerm.app".into()),
            proto_version: PANEL_PROTO_VERSION,
        };
        let line = serde_json::to_string(&PanelFrame::Hello { session: s }).unwrap();
        assert!(line.contains("\"type\":\"hello\""));
        assert!(matches!(
            decode_line::<PanelFrame>(&line),
            Some(PanelFrame::Hello { .. })
        ));
        let line = serde_json::to_string(&PanelFrame::Panel {
            focus: PanelTab::Preview,
        })
        .unwrap();
        assert!(line.contains("\"focus\":\"preview\""));
        let line = serde_json::to_string(&HubFrame::Insert { text: "/help".into() }).unwrap();
        assert!(matches!(
            decode_line::<HubFrame>(&line),
            Some(HubFrame::Insert { text }) if text == "/help"
        ));
    }

    #[test]
    fn unknown_frames_are_skipped_not_errors() {
        // Forward compat: a newer peer's frame type is ignored by an old peer.
        assert!(decode_line::<PanelFrame>(r#"{"type":"from_the_future"}"#).is_none());
        assert!(decode_line::<HubFrame>(r#"{"type":"execute","cmd":"rm"}"#).is_none());
        assert!(decode_line::<HubFrame>("not json").is_none());
    }
}
```

- [ ] **Step 1.2: Run to verify it fails**

Run: `cargo test -p mur-common panel`
Expected: FAIL — `murmur_run_dir`/types not found.

- [ ] **Step 1.3: Implementation** — prepend above the tests in `mur-common/src/panel.rs`:

```rust
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PANEL_PROTO_VERSION: u32 = 1;

/// Directory holding one `<pid>.json` + `<pid>.sock` per live murmur session.
/// Under the existing `~/.mur/runtime` convention (see `media::runtime_dir`).
pub fn murmur_run_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("murmur")
}

/// On-disk session record (`<pid>.json`) and the `Hello` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelSession {
    pub pid: u32,
    pub agent: String,
    pub cwd: String,
    pub sock: String,
    #[serde(default)]
    pub terminal_program: Option<String>,
    pub proto_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelTab {
    Information,
    Activities,
    Preview,
    Notifications,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    File,
    Url,
}

/// murmur → Hub frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PanelFrame {
    /// Sent once per connection, immediately on accept.
    Hello { session: PanelSession },
    /// Reserved for later phases (cwd/agent changes mid-session).
    State { cwd: String, agent: String },
    /// `/panel <tab>` — open/focus the Panel window on this tab.
    Panel { focus: PanelTab },
    /// `/panel preview <target>` — set the preview target (rendered in P3).
    Preview { kind: PreviewKind, target: String },
    Bye,
}

/// Hub → murmur frames. Insert-only by design: nothing here may ever
/// execute — `Insert` fills the input box and the user presses Enter
/// (fail-closed; spec §Security).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubFrame {
    Insert { text: String },
}

/// Tolerant line decode: `None` for unknown or malformed frames so old
/// peers keep working against newer senders.
pub fn decode_line<T: serde::de::DeserializeOwned>(line: &str) -> Option<T> {
    serde_json::from_str(line).ok()
}
```

- [ ] **Step 1.4: Run to verify it passes**

Run: `cargo test -p mur-common panel`
Expected: 3 passed.

- [ ] **Step 1.5: Amend the spec path** — in `docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md`, replace every `~/.mur/run/murmur/` with `~/.mur/runtime/murmur/` (aligns with the existing `~/.mur/runtime` convention from `mur-common::media::runtime_dir`).

- [ ] **Step 1.6: Commit**

```bash
git add mur-common/src/panel.rs mur-common/src/lib.rs docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md
git commit -m "feat(panel): murmur Panel wire protocol types in mur-common

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: murmur socket server — `cli/panel.rs`

**Files:**
- Create: `mur-core/src/cmd/agent/cli/panel.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (add `mod panel;` beside the other `mod` lines, ~line 40; no lib.rs/main.rs edits — this is inside the cli module tree)

**Interfaces:**
- Consumes: Task 1 types.
- Produces: `panel::start(home: &Path, agent: &str, cwd: &Path) -> (tokio::sync::mpsc::Receiver<HubFrame>, PanelHandle)`; `PanelHandle::send(&self, PanelFrame)` (fire-and-forget); `PanelHandle` is `Drop`-cleaned (removes json + sock). The returned receiver NEVER closes (handle holds a keepalive sender) — safe as a `tokio::select!` arm.

- [ ] **Step 2.1: Write the failing test** — create `mur-core/src/cmd/agent/cli/panel.rs` starting with the test module, and add `mod panel;` to `cli/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::panel::{PanelFrame, PanelSession, murmur_run_dir};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn hello_insert_roundtrip_and_drop_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut rx, handle) = start(tmp.path(), "tester", std::path::Path::new("/tmp"));
        let dir = murmur_run_dir(tmp.path());
        let pid = std::process::id();
        let json = dir.join(format!("{pid}.json"));
        let sock = dir.join(format!("{pid}.sock"));
        assert!(json.exists(), "session json written");

        let stream = UnixStream::connect(&sock).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut lines = BufReader::new(r).lines();

        // Server greets with Hello carrying the session record.
        let line = lines.next_line().await.unwrap().unwrap();
        let PanelFrame::Hello { session } =
            mur_common::panel::decode_line::<PanelFrame>(&line).unwrap()
        else {
            panic!("expected hello, got: {line}")
        };
        assert_eq!(session.agent, "tester");
        let parsed: PanelSession =
            serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
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
```

- [ ] **Step 2.2: Run to verify it fails**

Run: `cargo test -p mur-core panel::tests::hello_insert`
Expected: FAIL — `start` not found. (If the whole crate fails to compile for unrelated env reasons, re-check the env preamble.)

- [ ] **Step 2.3: Implementation** — prepend in `panel.rs`:

```rust
//! Panel bridge server: one Unix socket per murmur session that the MUR Hub
//! connects to. murmur pushes `PanelFrame`s; the Hub pushes `HubFrame`s
//! (insert-only). The session record + socket live under
//! `murmur_run_dir(home)` and vanish when the TUI exits — a vanished socket
//! is how the Hub learns the session ended.

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
    /// Fire-and-forget toward the Hub. Frames sent before a client connects
    /// buffer up to `CHANNEL_CAP` and flush on connect; overflow drops.
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

/// Start the per-session panel server. Never fatal: on bind failure the
/// handle still works, frames just go nowhere (the TUI must not care).
/// Must be called within a tokio runtime.
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
    std::fs::write(json_path, serde_json::to_vec(&session)?).context("write session json")?;
    tokio::spawn(accept_loop(listener, session, in_tx, out_rx));
    Ok(())
}

/// One client at a time; when a client disconnects we accept the next
/// (Hub restart). A second concurrent connect waits in the accept backlog.
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
        if !handle_client(stream, &session, &in_tx, &mut out_rx).await {
            return; // TUI side gone
        }
    }
}

/// Returns false when the TUI's outgoing channel closed (time to stop).
async fn handle_client(
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

async fn write_line(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    f: &PanelFrame,
) -> std::io::Result<()> {
    let mut buf = serde_json::to_vec(f).map_err(std::io::Error::other)?;
    buf.push(b'\n');
    w.write_all(&buf).await
}
```

- [ ] **Step 2.4: Run to verify it passes**

Run: `cargo test -p mur-core panel::tests::hello_insert`
Expected: 1 passed.

- [ ] **Step 2.5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/panel.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(panel): per-session unix socket server in murmur TUI

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: murmur TUI wiring — `/panel`, insert, autocomplete

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (SlashCmd + parse_slash, ~line 116-160, tests ~line 900)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (HELP ~line 98, event_loop ~line 355, call site ~line 307, handle_slash ~line 937, and the fn containing the `parse_slash` call at ~line 841)
- Modify: `mur-core/src/cmd/agent/cli/complete.rs` (COMMANDS ~line 40, test)
- Modify: `mur-core/src/cmd/agent/cli/panel.rs` (add `handle_panel_cmd` + `absolutize`)

**Interfaces:**
- Consumes: `panel::start`, `PanelHandle::send` (Task 2); `App::{set_input, push_system, cwd}` (existing).
- Produces: `SlashCmd::Panel(Vec<String>)`; `panel::handle_panel_cmd(app: &mut App, panel: &PanelHandle, args: &[String])`; select-arm insert behavior.

- [ ] **Step 3.1: Write the failing tests**

In `app.rs` tests (beside the existing `parse_slash` tests ~line 900):

```rust
#[test]
fn parses_panel() {
    assert_eq!(parse_slash("/panel"), Some(SlashCmd::Panel(vec![])));
    assert_eq!(
        parse_slash("/panel preview out/report.html"),
        Some(SlashCmd::Panel(vec![
            "preview".to_string(),
            "out/report.html".to_string()
        ]))
    );
}
```

In `complete.rs` tests (mirror the existing `/mcp` subcommand test ~line 241):

```rust
#[test]
fn panel_subcommands() {
    let s = compute("/panel ", &[]).unwrap();
    assert!(s.items.iter().any(|c| c.insert == "/panel preview "));
    assert_eq!(s.items.len(), 4);
}
```

In `panel.rs` tests:

```rust
#[test]
fn absolutize_paths() {
    assert_eq!(absolutize(Path::new("/repo"), "out/x.html"), "/repo/out/x.html");
    assert_eq!(absolutize(Path::new("/repo"), "/abs/x.html"), "/abs/x.html");
}
```

- [ ] **Step 3.2: Run to verify they fail**

Run: `cargo test -p mur-core parses_panel panel_subcommands absolutize` (three invocations or one filter each)
Expected: compile FAIL — `SlashCmd::Panel` / `absolutize` not found.

- [ ] **Step 3.3: Implement**

`app.rs` — add variant + parse arm:

```rust
    /// `/panel [tab] [target]` — open/drive the MUR Hub companion window.
    Panel(Vec<String>),
```

```rust
        "panel" => SlashCmd::Panel(words.map(str::to_string).collect()),
```

`complete.rs` — insert into `COMMANDS` between `mcp` and `quit`:

```rust
    (
        "panel",
        "companion window (MUR Hub)",
        &["information", "activities", "preview", "notifications"],
    ),
```

`mod.rs` HELP const — after `/skill` add ` /panel [tab]` so it reads `… /mcp  /skill  /panel [tab]  /exit …`.

`panel.rs` — append:

```rust
const PANEL_HINT: &str =
    "usage: /panel [information|activities|preview|notifications] · /panel preview <path|url>";

/// Handle `/panel …`: translate args to a frame, make sure the Hub is up,
/// send. Everything is fire-and-forget; the Hub opens/focuses the window
/// when the frame arrives.
pub fn handle_panel_cmd(app: &mut super::app::App, panel: &PanelHandle, args: &[String]) {
    use mur_common::panel::{PanelTab, PreviewKind};
    let frame = match args.first().map(String::as_str) {
        None | Some("information") | Some("info") => PanelFrame::Panel {
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
                target: absolutize(
                    app.cwd.as_deref().unwrap_or(Path::new(".")),
                    t,
                ),
            },
            None => PanelFrame::Panel {
                focus: PanelTab::Preview,
            },
        },
        Some(other) => {
            app.push_system(format!("unknown panel tab: {other} — {PANEL_HINT}"));
            return;
        }
    };
    ensure_hub_running(app);
    panel.send(frame);
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
```

`mod.rs` — wire the server and the select arm:

At the `event_loop` call site (~line 307):

```rust
    let cwd = app.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
    let (panel_rx, panel) = panel::start(&app.home, &app.agent, &cwd);
    let result = event_loop(&mut terminal, &mut app, panel_rx, &panel).await;
```

`event_loop` signature + new arm (the receiver never closes — keepalive in the handle — so this arm can't spin):

```rust
async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    mut panel_rx: mpsc::Receiver<mur_common::panel::HubFrame>,
    panel: &panel::PanelHandle,
) -> Result<()> {
```

```rust
            Some(hub) = panel_rx.recv() => match hub {
                mur_common::panel::HubFrame::Insert { text } => app.set_input(&text),
            },
```

Thread `panel: &panel::PanelHandle` down the call chain to `handle_slash`: add the parameter to `handle_event`, to the fn containing the `parse_slash` call at ~line 841 (likely `submit` or the key handler), and to `handle_slash`; let the compiler list every call site. Then in `handle_slash` add:

```rust
        SlashCmd::Panel(args) => panel::handle_panel_cmd(app, panel, &args),
```

Note: `app.cwd` is `Option<PathBuf>`; `as_deref()` gives `Option<&Path>`.

- [ ] **Step 3.4: Run to verify green**

Run: `cargo test -p mur-core cli`
Expected: all cli-module tests pass (including the three new ones and Task 2's).

- [ ] **Step 3.5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/
git commit -m "feat(panel): /panel command, insert handling, autocomplete in murmur

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Hub-side bridge — `mur-gui-core::panel_bridge`

**Files:**
- Create: `mur-gui-core/src/panel_bridge/mod.rs`
- Create: `mur-gui-core/src/panel_bridge/client.rs`
- Modify: `mur-gui-core/src/lib.rs` (add `pub mod panel_bridge;` after `pub mod oauth_bridge;`)

**Interfaces:**
- Consumes: Task 1 types; `notify` 6 + tokio `net` (already gui-core deps).
- Produces: `mur_gui_core::panel_bridge::{PanelEvent, PanelBridge}`:
  - `enum PanelEvent { Frame { pid: u32, frame: PanelFrame }, SessionDown { pid: u32 } }`
  - `PanelBridge::start(mur_home: PathBuf, tx: mpsc::Sender<PanelEvent>) -> Result<PanelBridge>` (must be called inside a tokio runtime)
  - `PanelBridge::insert(&self, pid: u32, text: String) -> bool`

- [ ] **Step 4.1: Write the failing test** — in `panel_bridge/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::panel::{
        HubFrame, PANEL_PROTO_VERSION, PanelFrame, PanelSession, murmur_run_dir,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;
    use tokio::time::{Duration, timeout};

    /// Minimal murmur stand-in: session json + socket that greets with Hello,
    /// then echoes nothing and records one inbound line.
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
    async fn discovers_connects_inserts_and_reports_down() {
        let tmp = tempfile::tempdir().unwrap();
        let (listener, _json) = fake_murmur(tmp.path(), 7001).await;
        let (tx, mut rx) = mpsc::channel(16);
        let bridge = PanelBridge::start(tmp.path().to_path_buf(), tx).unwrap();

        // Bridge connects (initial scan) → fake server sends Hello.
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

        let ev = timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        let PanelEvent::Frame { pid: 7001, frame: PanelFrame::Panel { .. } } = ev else {
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
        let ev = timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
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
        // Connect fails (no socket) → record reaped.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!json.exists());
    }
}
```

- [ ] **Step 4.2: Run to verify it fails**

Run: `cargo test -p mur-gui-core panel_bridge`
Expected: compile FAIL — module not found. (Add `tempfile` to gui-core `[dev-dependencies]` if missing: `tempfile = "3"`.)

- [ ] **Step 4.3: Implement** — add `pub mod panel_bridge;` to `mur-gui-core/src/lib.rs` (after `pub mod oauth_bridge;`), then `panel_bridge/mod.rs`:

```rust
//! Panel bridge (Hub side): discovers live murmur TUI sessions — one
//! `<pid>.json` + `<pid>.sock` under `murmur_run_dir` — and keeps one socket
//! client per session. Frames are republished as [`PanelEvent`]s; the only
//! send path is [`PanelBridge::insert`] (insert-only by protocol design).

mod client;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use mur_common::panel::{HubFrame, PanelFrame, murmur_run_dir};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum PanelEvent {
    /// Any frame from a session, including the initial `Hello`.
    Frame { pid: u32, frame: PanelFrame },
    SessionDown { pid: u32 },
}

pub(crate) type Senders = Arc<Mutex<HashMap<u32, mpsc::Sender<HubFrame>>>>;

pub struct PanelBridge {
    senders: Senders,
    _watcher: RecommendedWatcher,
}

impl PanelBridge {
    /// Scan for existing sessions and watch for new ones. Must be called
    /// from within a tokio runtime (spawns the per-session client tasks).
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
            Config::default(),
        )
        .context("notify::watcher::new")?;
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

    /// Queue an insert-only frame toward one session's input box.
    /// Returns false when the session is gone (or its queue is full).
    pub fn insert(&self, pid: u32, text: String) -> bool {
        self.senders
            .lock()
            .unwrap()
            .get(&pid)
            .map(|s| s.try_send(HubFrame::Insert { text }).is_ok())
            .unwrap_or(false)
    }
}
```

`panel_bridge/client.rs`:

```rust
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
            tracing::warn!("panel_bridge: malformed session record {}", json_path.display());
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
                        }
                    }
                    _ => break, // EOF: session over
                },
                Some(f) = out_rx.recv() => {
                    let Ok(mut buf) = serde_json::to_vec(&f) else { break };
                    buf.push(b'\n');
                    if w.write_all(&buf).await.is_err() {
                        break;
                    }
                }
            }
        }
        senders.lock().unwrap().remove(&pid);
        let _ = tx.send(PanelEvent::SessionDown { pid }).await;
    });
}
```

- [ ] **Step 4.4: Run to verify green**

Run: `cargo test -p mur-gui-core panel_bridge`
Expected: 2 passed.

- [ ] **Step 4.5: Commit**

```bash
git add mur-gui-core/src/panel_bridge/ mur-gui-core/src/lib.rs mur-gui-core/Cargo.toml
git commit -m "feat(panel): session discovery + socket client bridge in mur-gui-core

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Window positioning seam — `panel/pos.rs`

**Files:**
- Create: `mur-hub-gui/src-tauri/src/panel/mod.rs` (stub: `pub mod pos;` only, filled in Task 6)
- Create: `mur-hub-gui/src-tauri/src/panel/pos.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (add `mod panel;` beside the other `mod` lines)
- Modify: `mur-hub-gui/src-tauri/Cargo.toml` (macOS dep)

**Interfaces:**
- Consumes: `crate::geometry::{Rect, anchor_panel, clamp_into}`; `crate::pet::monitor_rect_for_point(&AppHandle, i32, i32) -> Rect` (both existing).
- Produces: `panel::pos::{PANEL_W: f64, PANEL_H: f64, reposition(&tauri::WebviewWindow, Option<crate::geometry::Rect>), terminal_window_bounds(&tauri::WebviewWindow, &str) -> Option<crate::geometry::Rect>, owner_name_for(&str) -> &str}`.

- [ ] **Step 5.1: Cargo dep** — in `mur-hub-gui/src-tauri/Cargo.toml` add (create the target section if absent):

```toml
[target.'cfg(target_os = "macos")'.dependencies]
core-foundation = "0.10"
```

- [ ] **Step 5.2: Write the failing test** — in `panel/pos.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_map_covers_known_terminals() {
        assert_eq!(owner_name_for("Apple_Terminal"), "Terminal");
        assert_eq!(owner_name_for("iTerm.app"), "iTerm2");
        assert_eq!(owner_name_for("WezTerm"), "wezterm-gui");
        assert_eq!(owner_name_for("ghostty"), "Ghostty");
        assert_eq!(owner_name_for("kitty"), "kitty");
        // Unknown values pass through — CG matching is substring/CI anyway.
        assert_eq!(owner_name_for("rio"), "rio");
    }
}
```

- [ ] **Step 5.3: Run to verify it fails**

Run (with ui/dist stub in place): `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml owner_map`
Expected: compile FAIL.

- [ ] **Step 5.4: Implement** — add `mod panel;` to `mur-hub-gui/src-tauri/src/lib.rs` (beside the other `mod` declarations), then `panel/mod.rs`:

```rust
//! Murmur Panel window (companion). P1: pos.rs only; lifecycle lands next.
pub mod pos;
```

`panel/pos.rs`:

```rust
//! Panel window placement. [`reposition`] is the SINGLE placement primitive:
//! snap-once calls it at open; future live-follow (AXObserver, spec §Window
//! Positioning) re-invokes it on terminal move/resize. Nothing else may set
//! the panel window's position.

use tauri::Manager;

use crate::geometry::{Rect, anchor_panel, clamp_into};

pub const PANEL_W: f64 = 360.0;
pub const PANEL_H: f64 = 560.0;
/// Fallback inset from the screen's top-right corner, physical px.
const FALLBACK_MARGIN: i32 = 16;

/// `TERM_PROGRAM` value → CGWindow owner-name needle.
pub fn owner_name_for(term_program: &str) -> &str {
    match term_program {
        "Apple_Terminal" => "Terminal",
        "iTerm.app" => "iTerm2",
        "WezTerm" => "wezterm-gui",
        "ghostty" => "Ghostty",
        "kitty" => "kitty",
        other => other,
    }
}

/// Place the panel beside `target` (a terminal window's bounds, physical px)
/// or at the primary screen's right edge when `None`. Clamped on-screen.
pub fn reposition(win: &tauri::WebviewWindow, target: Option<Rect>) {
    let scale = win.scale_factor().unwrap_or(1.0);
    let size = ((PANEL_W * scale) as i32, (PANEL_H * scale) as i32);
    let pos = match target {
        Some(t) => {
            let mon = crate::pet::monitor_rect_for_point(win.app_handle(), t.x, t.y);
            anchor_panel(t, size, mon)
        }
        None => {
            let Ok(Some(m)) = win.primary_monitor() else {
                return;
            };
            let mon = Rect {
                x: m.position().x,
                y: m.position().y,
                w: m.size().width as i32,
                h: m.size().height as i32,
            };
            clamp_into(
                (
                    mon.right() - size.0 - FALLBACK_MARGIN,
                    mon.y + FALLBACK_MARGIN * 4,
                ),
                size,
                mon,
            )
        }
    };
    let _ = win.set_position(tauri::PhysicalPosition::new(pos.0, pos.1));
}

/// Frontmost window bounds (physical px) of the terminal app named by
/// `TERM_PROGRAM`. No Accessibility / Screen Recording permission needed:
/// CGWindowList exposes bounds (only window *titles* are gated).
#[cfg(target_os = "macos")]
pub fn terminal_window_bounds(win: &tauri::WebviewWindow, term_program: &str) -> Option<Rect> {
    // CG bounds are in logical points (global, top-left origin); tauri
    // positions are physical px. Scale by the panel's monitor factor —
    // right for the common same-monitor case; multi-monitor mixed-DPI
    // refinement can ride the live-follow work.
    let scale = win.scale_factor().unwrap_or(1.0);
    let (x, y, w, h) = cg::frontmost_window_bounds(owner_name_for(term_program))?;
    Some(Rect {
        x: (x * scale) as i32,
        y: (y * scale) as i32,
        w: (w * scale) as i32,
        h: (h * scale) as i32,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn terminal_window_bounds(_win: &tauri::WebviewWindow, _term_program: &str) -> Option<Rect> {
    None
}

#[cfg(target_os = "macos")]
mod cg {
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(
            option: u32,
            relative_to: u32,
        ) -> CFArrayRef;
    }
    const ON_SCREEN_ONLY: u32 = 1 << 0; // kCGWindowListOptionOnScreenOnly
    const EXCLUDE_DESKTOP: u32 = 1 << 4; // kCGWindowListExcludeDesktopElements

    /// First (= frontmost; the list is front-to-back) layer-0 window whose
    /// owner name contains `owner` (case-insensitive). Returns logical
    /// points (x, y, w, h).
    pub fn frontmost_window_bounds(owner: &str) -> Option<(f64, f64, f64, f64)> {
        let raw = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) };
        if raw.is_null() {
            return None;
        }
        let arr: CFArray<CFDictionary<CFString, CFType>> =
            unsafe { CFArray::wrap_under_create_rule(raw as _) };
        let want = owner.to_lowercase();
        for dict in arr.iter() {
            let owner_name = dict
                .find(CFString::from_static_string("kCGWindowOwnerName"))
                .and_then(|v| v.downcast::<CFString>())
                .map(|s| s.to_string().to_lowercase());
            if !owner_name.is_some_and(|n| n.contains(&want)) {
                continue;
            }
            let layer = dict
                .find(CFString::from_static_string("kCGWindowLayer"))
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i32());
            if layer != Some(0) {
                continue; // skip menubar/status-item windows
            }
            let bounds = dict
                .find(CFString::from_static_string("kCGWindowBounds"))
                .and_then(|v| v.downcast::<CFDictionary>())?;
            let num = |k: &'static str| {
                bounds
                    .find(CFString::from_static_string(k).as_CFType())
                    .and_then(|v| {
                        unsafe {
                            CFNumber::wrap_under_get_rule(v.as_CFTypeRef() as _)
                        }
                        .to_f64()
                    })
            };
            if let (Some(x), Some(y), Some(w), Some(h)) =
                (num("X"), num("Y"), num("Width"), num("Height"))
            {
                return Some((x, y, w, h));
            }
        }
        None
    }
}
```

Note for the implementer: the `cg` module is the one place where the
core-foundation API surface may need small adjustments (`find` key/value
typing differs across cf versions) — iterate against
`cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` until green,
keeping the public signature `frontmost_window_bounds(&str) -> Option<(f64, f64, f64, f64)>` fixed. If the typed-dictionary route fights back, drop to
`CFDictionaryGetValue` raw calls inside this module only.

- [ ] **Step 5.5: Run to verify green**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml owner_map`
Expected: 1 passed (and the crate compiles).

- [ ] **Step 5.6: Commit**

```bash
git add mur-hub-gui/src-tauri/src/panel/ mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/Cargo.toml mur-hub-gui/src-tauri/Cargo.lock
git commit -m "feat(panel): reposition seam + CGWindowList terminal bounds lookup

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Hub backend — window lifecycle, frame routing, commands

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/panel/mod.rs` (replace stub)
- Create: `mur-hub-gui/src-tauri/capabilities/panel.json`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (manage state ~line 257, setup ~line 330, invoke_handler ~line 561)

**Interfaces:**
- Consumes: Task 4 `PanelBridge`/`PanelEvent`; Task 5 `pos::{reposition, terminal_window_bounds, PANEL_W, PANEL_H}`; `chat_window.rs` window-builder pattern.
- Produces: window label `"murmur-panel"` at route `index.html#/panel`; Tauri commands `panel_sessions() -> Vec<PanelSession>`, `panel_insert(pid, text) -> Result<(), String>`, `open_panel_window()`; global events `"panel-sessions"` (payload `Vec<PanelSession>`), `"panel-focus"` (`{pid, tab}`), `"panel-preview"` (`{pid, kind, target}`).

- [ ] **Step 6.1: Implement `panel/mod.rs`:**

```rust
//! Murmur Panel window: lifecycle + frame routing. One always-on-top window
//! (label `murmur-panel`) bound to live murmur TUI sessions discovered by
//! `mur_gui_core::panel_bridge`. All Hub→murmur traffic is insert-only.

pub mod pos;

use std::collections::HashMap;
use std::sync::Mutex;

use mur_common::panel::{PanelFrame, PanelSession};
use mur_gui_core::panel_bridge::{PanelBridge, PanelEvent};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::mpsc;

pub const PANEL_LABEL: &str = "murmur-panel";

#[derive(Default)]
pub struct PanelState {
    bridge: Mutex<Option<PanelBridge>>,
    sessions: Mutex<HashMap<u32, PanelSession>>,
}

/// Start the bridge and pump its events. Called from `setup` (inside the
/// entered runtime — `PanelBridge::start` requires it).
pub fn spawn_bridge(app: AppHandle, mur_home: std::path::PathBuf) {
    let (tx, mut rx) = mpsc::channel::<PanelEvent>(64);
    match PanelBridge::start(mur_home, tx) {
        Ok(bridge) => *app.state::<PanelState>().bridge.lock().unwrap() = Some(bridge),
        Err(e) => {
            tracing::warn!("panel: bridge disabled: {e:#}");
            return;
        }
    }
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                PanelEvent::Frame { pid, frame } => on_frame(&app, pid, frame),
                PanelEvent::SessionDown { pid } => {
                    app.state::<PanelState>().sessions.lock().unwrap().remove(&pid);
                    emit_sessions(&app);
                }
            }
        }
    });
}

fn on_frame(app: &AppHandle, pid: u32, frame: PanelFrame) {
    match frame {
        PanelFrame::Hello { session } => {
            app.state::<PanelState>()
                .sessions
                .lock()
                .unwrap()
                .insert(pid, session);
            emit_sessions(app);
        }
        PanelFrame::State { cwd, agent } => {
            if let Some(s) = app.state::<PanelState>().sessions.lock().unwrap().get_mut(&pid) {
                s.cwd = cwd;
                s.agent = agent;
            }
            emit_sessions(app);
        }
        PanelFrame::Panel { focus } => {
            open_or_focus(app, Some(pid));
            let _ = app.emit("panel-focus", serde_json::json!({ "pid": pid, "tab": focus }));
        }
        PanelFrame::Preview { kind, target } => {
            open_or_focus(app, Some(pid));
            let _ = app.emit(
                "panel-focus",
                serde_json::json!({ "pid": pid, "tab": "preview" }),
            );
            let _ = app.emit(
                "panel-preview",
                serde_json::json!({ "pid": pid, "kind": kind, "target": target }),
            );
        }
        PanelFrame::Bye => {}
    }
}

fn emit_sessions(app: &AppHandle) {
    let list: Vec<PanelSession> = app
        .state::<PanelState>()
        .sessions
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    let _ = app.emit("panel-sessions", list);
}

/// Open the panel window (snap-once beside `pid`'s terminal) or re-show it.
fn open_or_focus(app: &AppHandle, pid: Option<u32>) {
    if let Some(win) = app.get_webview_window(PANEL_LABEL) {
        let _ = win.show();
        return; // no set_focus: never steal focus from the terminal
    }
    let win = match WebviewWindowBuilder::new(
        app,
        PANEL_LABEL,
        WebviewUrl::App("index.html#/panel".into()),
    )
    .title("MUR Panel")
    .inner_size(pos::PANEL_W, pos::PANEL_H)
    .min_inner_size(300.0, 400.0)
    .resizable(true)
    .always_on_top(true)
    .visible(false)
    .build()
    {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("panel: window create failed: {e}");
            return;
        }
    };
    // Snap-once: bounds of the session's terminal, else screen right edge.
    let target = pid
        .and_then(|p| {
            app.state::<PanelState>()
                .sessions
                .lock()
                .unwrap()
                .get(&p)
                .and_then(|s| s.terminal_program.clone())
        })
        .and_then(|tp| pos::terminal_window_bounds(&win, &tp));
    pos::reposition(&win, target);
    let _ = win.show();
}

#[tauri::command]
pub fn panel_sessions(state: State<PanelState>) -> Vec<PanelSession> {
    state.sessions.lock().unwrap().values().cloned().collect()
}

#[tauri::command]
pub fn panel_insert(pid: u32, text: String, state: State<PanelState>) -> Result<(), String> {
    let ok = state
        .bridge
        .lock()
        .unwrap()
        .as_ref()
        .map(|b| b.insert(pid, text))
        .unwrap_or(false);
    if ok { Ok(()) } else { Err("session gone".into()) }
}

#[tauri::command]
pub fn open_panel_window(app: AppHandle, state: State<PanelState>) -> Result<(), String> {
    let latest = state.sessions.lock().unwrap().keys().max().copied();
    open_or_focus(&app, latest);
    Ok(())
}
```

- [ ] **Step 6.2: Capability** — create `mur-hub-gui/src-tauri/capabilities/panel.json` (without this the window loses IPC — pet lesson, PR #401):

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "identifier": "panel",
  "description": "Capability set for the murmur Panel companion window (murmur-panel label): listen for panel events, invoke panel commands, basic window controls.",
  "windows": ["murmur-panel"],
  "permissions": [
    "core:default",
    "core:event:allow-listen",
    "core:event:allow-unlisten",
    "core:window:allow-close",
    "core:window:allow-start-dragging",
    "core:window:allow-set-size",
    "core:window:allow-set-focus"
  ]
}
```

- [ ] **Step 6.3: Wire `lib.rs`** — three insertions:

In the `.manage(...)` chain (~line 257): `.manage(panel::PanelState::default())`

In `setup` after `spawn_runtime_watcher(...)` (~line 330, inside the entered-runtime section):

```rust
            // Murmur Panel: discover TUI sessions and route their frames.
            panel::spawn_bridge(app.handle().clone(), mur_home.clone());
```

In `invoke_handler` (~line 561): `panel::panel_sessions, panel::panel_insert, panel::open_panel_window,`

- [ ] **Step 6.4: Verify compile**

Run: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: green (ui/dist stub in place).

- [ ] **Step 6.5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/panel/ mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/capabilities/panel.json
git commit -m "feat(panel): Hub panel window lifecycle, frame routing, commands

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Frontend — PanelWindow

**Files:**
- Create: `mur-hub-gui/ui/src/components/panel/PanelWindow.tsx`
- Create: `mur-hub-gui/ui/src/components/panel/panel.css`
- Modify: `mur-hub-gui/ui/src/App.tsx` (route)

**Interfaces:**
- Consumes: commands + events from Task 6 (exact payload shapes there).
- Produces: `#/panel` route rendering four tabs; the P1 demo affordance (test-insert button).

- [ ] **Step 7.1: Route** — in `App.tsx`, extend `getRoute()` and the render switch:

```tsx
  if (hash.startsWith("#/panel")) return "panel";
```
```tsx
  if (route === "panel") return <PanelWindow />;
```
(plus `import { PanelWindow } from "./components/panel/PanelWindow";` and the union type member `"panel"`).

- [ ] **Step 7.2: Component** — `components/panel/PanelWindow.tsx`:

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./panel.css";

type PanelSession = {
  pid: number;
  agent: string;
  cwd: string;
  terminal_program?: string | null;
};
type Tab = "information" | "activities" | "preview" | "notifications";
const TABS: Tab[] = ["information", "activities", "preview", "notifications"];
const TAB_LABEL: Record<Tab, string> = {
  information: "Info",
  activities: "Activities",
  preview: "Preview",
  notifications: "Notifications",
};

export function PanelWindow() {
  const [sessions, setSessions] = useState<PanelSession[]>([]);
  const [pid, setPid] = useState<number | null>(null);
  const [tab, setTab] = useState<Tab>("information");
  const [previewTarget, setPreviewTarget] = useState<string | null>(null);

  useEffect(() => {
    void invoke<PanelSession[]>("panel_sessions").then((s) => {
      setSessions(s);
      setPid((cur) => cur ?? (s.length ? s[s.length - 1].pid : null));
    });
    const unSessions = listen<PanelSession[]>("panel-sessions", (e) => {
      setSessions(e.payload);
      setPid((cur) =>
        cur !== null && e.payload.some((s) => s.pid === cur)
          ? cur
          : e.payload.length
            ? e.payload[e.payload.length - 1].pid
            : null,
      );
    });
    const unFocus = listen<{ pid: number; tab: Tab }>("panel-focus", (e) => {
      setPid(e.payload.pid);
      setTab(e.payload.tab);
    });
    const unPreview = listen<{ pid: number; kind: string; target: string }>(
      "panel-preview",
      (e) => setPreviewTarget(e.payload.target),
    );
    return () => {
      void unSessions.then((f) => f());
      void unFocus.then((f) => f());
      void unPreview.then((f) => f());
    };
  }, []);

  const sess = sessions.find((s) => s.pid === pid) ?? null;
  const testInsert = () => {
    if (pid !== null) void invoke("panel_insert", { pid, text: "/help" }).catch(() => {});
  };

  return (
    <div className="panel-root">
      <header className="panel-header">
        <span className="panel-title">MUR Panel</span>
        <select
          value={pid ?? ""}
          onChange={(e) => setPid(e.target.value ? Number(e.target.value) : null)}
        >
          {sessions.map((s) => (
            <option key={s.pid} value={s.pid}>
              {s.agent} · {s.pid}
            </option>
          ))}
        </select>
      </header>
      <nav className="panel-tabs">
        {TABS.map((t) => (
          <button
            key={t}
            className={t === tab ? "panel-tab active" : "panel-tab"}
            onClick={() => setTab(t)}
          >
            {TAB_LABEL[t]}
          </button>
        ))}
      </nav>
      <main className="panel-body">
        {!sess ? (
          <p className="panel-empty">
            No live murmur session — run <code>murmur</code> and type <code>/panel</code>.
          </p>
        ) : tab === "information" ? (
          <div>
            <dl className="panel-info">
              <dt>Agent</dt>
              <dd>{sess.agent}</dd>
              <dt>Working dir</dt>
              <dd>{sess.cwd}</dd>
              <dt>Terminal</dt>
              <dd>{sess.terminal_program ?? "unknown"}</dd>
            </dl>
            {/* P1 demo affordance; replaced by real recommendations in P2. */}
            <button className="panel-test" onClick={testInsert}>
              Insert /help into murmur
            </button>
          </div>
        ) : tab === "preview" && previewTarget ? (
          <p className="panel-empty">
            Preview target: <code>{previewTarget}</code> (rendering lands in P3)
          </p>
        ) : (
          <p className="panel-empty">
            {TAB_LABEL[tab]} lands in {tab === "preview" ? "P3" : "P2"}.
          </p>
        )}
      </main>
    </div>
  );
}
```

- [ ] **Step 7.3: Styles** — `components/panel/panel.css` (minimal, dark-scheme-aware like sibling styles):

```css
.panel-root { display: flex; flex-direction: column; height: 100vh; font: 13px/1.5 -apple-system, sans-serif; }
.panel-header { display: flex; align-items: center; justify-content: space-between; padding: 8px 12px; gap: 8px; }
.panel-title { font-weight: 600; }
.panel-tabs { display: flex; gap: 4px; padding: 0 8px; border-bottom: 1px solid rgba(128, 128, 128, 0.25); }
.panel-tab { border: none; background: none; padding: 6px 10px; cursor: pointer; opacity: 0.65; }
.panel-tab.active { opacity: 1; font-weight: 600; border-bottom: 2px solid currentColor; }
.panel-body { flex: 1; overflow-y: auto; padding: 12px; }
.panel-empty { opacity: 0.65; }
.panel-info dt { font-weight: 600; margin-top: 8px; }
.panel-info dd { margin: 0; word-break: break-all; }
.panel-test { margin-top: 16px; padding: 6px 12px; cursor: pointer; }
```

- [ ] **Step 7.4: Verify build**

Run: `cd mur-hub-gui/ui && npm run build`
Expected: builds clean (tsc + vite). Fix any type errors.

- [ ] **Step 7.5: Commit**

```bash
git add mur-hub-gui/ui/src/components/panel/ mur-hub-gui/ui/src/App.tsx
git commit -m "feat(panel): PanelWindow four-tab UI + #/panel route

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Sweep, smoke, PR

- [ ] **Step 8.1: Format + lint everything (including excluded crates)**

```bash
cargo fmt --all
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo clippy -p mur-common -p mur-core -p mur-gui-core -- -D warnings
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- -D warnings
```
Expected: no diffs, no warnings. (Hub clippy needs the ui/dist stub.)

- [ ] **Step 8.2: Full test pass on touched crates**

```bash
cargo test -p mur-common && cargo test -p mur-core cli && cargo test -p mur-gui-core
```
Expected: green.

- [ ] **Step 8.3: Manual smoke (operator)** — requires a built Hub (`gotcha_hub_local_app_build_recipe`) and `cargo build -p mur-core`:
  1. `target/debug/mur agent cli mur` in iTerm2 → `/panel` → Panel window appears beside the terminal (right side; if offset is wrong, note the scale handling in `terminal_window_bounds`).
  2. Click "Insert /help into murmur" → `/help` appears in murmur's input box (NOT executed).
  3. `/panel activities` → tab switches. `/panel preview docs/README.md` → Preview tab shows the absolute target path.
  4. Quit murmur → Panel shows the no-session state; `~/.mur/runtime/murmur/` is empty.
  5. Restart Hub with murmur still running → session rediscovered.

- [ ] **Step 8.4: PR**

```bash
git push -u origin feat/murmur-panel-p1
gh pr create --title "feat(panel): murmur Panel companion window — P1 skeleton" --body "$(cat <<'EOF'
P1 of docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md:
per-session unix socket in murmur, session discovery + client bridge in
mur-gui-core, always-on-top MUR Panel window (four tabs) with snap-once
positioning behind a reposition() seam, /panel command, insert-only
Hub→murmur path.

P2 (data panels), P3 (preview rendering), P4 (recommendations/stream)
follow in separate plans.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Spec coverage map

| Spec section | Task |
|---|---|
| Frame types / proto version / tolerance | 1 |
| Transport & lifecycle (socket, session file, cleanup, buffering) | 2 |
| `/panel` command surface + autocomplete + insert-only click semantics | 3 |
| Discovery, reconnect, stale reaping, SessionDown | 4 |
| Window positioning (snap-once, `reposition` seam, no-permission bounds) | 5 |
| Panel window, capability, events, commands, session dropdown | 6, 7 |
| Security (0600/0700, insert-only, localhost-only preview) | 2 (perms), 1+3 (insert-only); preview URL restriction lands with rendering in P3 |
| Testing | each task + 8 |

Out of P1 (per spec phasing): Information git data, Activities, Notifications (P2); preview rendering + file watch (P3); recommendations, stream deltas (P4); AXObserver live-follow (future).
