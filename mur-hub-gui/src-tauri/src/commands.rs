//! Hub-wide Tauri state and the commands / background bridges that sit beside `run()`.

use crate::override_restart;
use mur_gui_core::discovery::AgentEntry;
use mur_gui_core::sidecar::{AgentRuntimeStatus, Supervisor};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

/// Managed Tauri state: current agent metadata snapshot.
pub struct AgentState(pub Mutex<Vec<AgentEntry>>);

/// Managed Tauri state: sidecar supervisor handle.
pub struct SupervisorState(pub Supervisor);

// ─── Tauri commands ────────────────────────────────────────────────────────

#[tauri::command]
pub fn list_agents(state: State<'_, AgentState>) -> Vec<AgentEntry> {
    state.0.lock().unwrap().clone()
}

/// The runtime snapshot as it stands right now.
///
/// `runtime-status-changed` only fires when the value *changes*, so a UI that
/// mounts after the supervisor has settled sees nothing and renders every agent
/// idle. `list_agents` above has always had this invoke-once-then-listen pair;
/// runtime status was listen-only, which is what left the Agents page showing a
/// resting flock while the agents were up.
#[tauri::command]
pub fn list_runtime_statuses(supervisor: State<'_, SupervisorState>) -> Vec<AgentRuntimeStatus> {
    supervisor.0.status_receiver().borrow().clone()
}

#[tauri::command]
pub async fn start_agent(
    name: String,
    supervisor: State<'_, SupervisorState>,
    jobs: State<'_, override_restart::jobs::OverrideJobs>,
) -> Result<(), String> {
    jobs.refuse_if_busy(&name)?;
    supervisor.0.start(&name).await
}

#[tauri::command]
pub async fn stop_agent(
    name: String,
    supervisor: State<'_, SupervisorState>,
    jobs: State<'_, override_restart::jobs::OverrideJobs>,
) -> Result<(), String> {
    jobs.refuse_if_busy(&name)?;
    supervisor.0.stop(&name).await;
    Ok(())
}

/// Show + focus the dashboard, then tell it where to go: `select-agent`
/// (existing), `select-fleet`, and `open-page` are each emitted only when
/// given. Detail windows use this as their "Show in Hub" / "Home" bridge.
#[tauri::command]
pub fn open_dashboard(
    app: AppHandle,
    agent_name: Option<String>,
    fleet_name: Option<String>,
    page: Option<String>,
) {
    let Some(win) = app.get_webview_window("dashboard") else {
        return;
    };
    let _ = win.show();
    let _ = win.set_focus();
    if let Some(name) = agent_name {
        let _ = app.emit("select-agent", name);
    }
    if let Some(name) = fleet_name {
        let _ = app.emit("select-fleet", name);
    }
    if let Some(id) = page {
        let _ = app.emit("open-page", id);
    }
}

#[tauri::command]
pub fn toggle_popover(app: AppHandle) {
    if let Some(win) = app.get_webview_window("popover") {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            position_popover(&app, &win);
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

// ─── Tray positioning ──────────────────────────────────────────────────────

fn position_popover(app: &AppHandle, win: &tauri::WebviewWindow) {
    let scale = win.scale_factor().unwrap_or(1.0);

    let (px, py) = app
        .tray_by_id("main")
        .and_then(|tray| tray.rect().ok().flatten())
        .map(|rect| {
            let pos = rect.position.to_physical::<i32>(scale);
            let sz = rect.size.to_physical::<u32>(scale);
            #[cfg(target_os = "macos")]
            {
                (pos.x, pos.y + sz.height as i32)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = sz;
                (pos.x, pos.y - 480)
            }
        })
        .unwrap_or_else(|| {
            win.primary_monitor()
                .ok()
                .flatten()
                .map(|m| {
                    let s = m.size();
                    (s.width as i32 - 300, 40)
                })
                .unwrap_or((20, 40))
        });

    let _ = win.set_position(tauri::PhysicalPosition::new(px, py));
}

// ─── Background event bridges ──────────────────────────────────────────────

pub(crate) fn spawn_agent_watcher(
    app: AppHandle,
    mut rx: tokio::sync::watch::Receiver<Vec<AgentEntry>>,
) {
    tokio::spawn(async move {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            let entries = rx.borrow().clone();
            if let Some(state) = app.try_state::<AgentState>() {
                *state.0.lock().unwrap() = entries.clone();
            }
            let _ = app.emit("agents-updated", &entries);
        }
    });
}

pub(crate) fn spawn_runtime_watcher(
    app: AppHandle,
    mut rx: tokio::sync::watch::Receiver<Vec<AgentRuntimeStatus>>,
) {
    tokio::spawn(async move {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            let statuses = rx.borrow().clone();
            let _ = app.emit("runtime-status-changed", &statuses);
        }
    });
}
