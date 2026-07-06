//! murmur Panel companion window: an always-on-top webview (label
//! `murmur-panel`) snapped beside the TUI's terminal. Bridge events route
//! here; clicks go back insert-only.

pub mod data;
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

/// Start the bridge and the event pump. Called from Tauri setup, inside
/// the runtime (`PanelBridge::start` requires it).
pub fn spawn_bridge(app: AppHandle, mur_home: std::path::PathBuf) {
    let (tx, mut rx) = mpsc::channel::<PanelEvent>(64);
    match PanelBridge::start(mur_home, tx) {
        Ok(bridge) => *app.state::<PanelState>().bridge.lock().unwrap() = Some(bridge),
        Err(e) => {
            tracing::warn!("panel bridge disabled: {e:#}");
            return;
        }
    }
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                PanelEvent::Frame { pid, frame } => on_frame(&app, pid, frame),
                PanelEvent::SessionDown { pid } => {
                    app.state::<PanelState>()
                        .sessions
                        .lock()
                        .unwrap()
                        .remove(&pid);
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
            if let Some(s) = app
                .state::<PanelState>()
                .sessions
                .lock()
                .unwrap()
                .get_mut(&pid)
            {
                s.cwd = cwd;
                s.agent = agent;
            }
            emit_sessions(app);
        }
        PanelFrame::Panel { focus } => {
            open_or_focus(app, Some(pid));
            let _ = app.emit(
                "panel-focus",
                serde_json::json!({ "pid": pid, "tab": focus }),
            );
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
        return;
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
            tracing::warn!("panel window: {e}");
            return;
        }
    };
    // Snap-once: place beside the requesting session's terminal window.
    let term = pid.and_then(|p| {
        app.state::<PanelState>()
            .sessions
            .lock()
            .unwrap()
            .get(&p)
            .and_then(|s| s.terminal_program.clone())
    });
    let target = term.and_then(|t| pos::terminal_window_bounds(&win, &t));
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
    if ok {
        Ok(())
    } else {
        Err("session gone".into())
    }
}

#[tauri::command]
pub fn open_panel_window(app: AppHandle, state: State<PanelState>) -> Result<(), String> {
    let latest = state.sessions.lock().unwrap().keys().max().copied();
    open_or_focus(&app, latest);
    Ok(())
}
