//! MuR Hub — Tauri 2 desktop app.
//!
//! M-h1: tray icon, popover + dashboard windows, agent discovery, global shortcut.

use mur_gui_core::discovery::{AgentDiscovery, AgentEntry};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{
    AppHandle, Emitter, Manager, State,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tracing_subscriber::EnvFilter;

/// Managed Tauri state: current snapshot of discovered agents.
pub struct AgentState(pub Mutex<Vec<AgentEntry>>);

// ─── Tauri commands ────────────────────────────────────────────────────────

#[tauri::command]
fn list_agents(state: State<'_, AgentState>) -> Vec<AgentEntry> {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
fn open_dashboard(app: AppHandle, agent_name: Option<String>) {
    let Some(win) = app.get_webview_window("dashboard") else {
        return;
    };
    let _ = win.show();
    let _ = win.set_focus();
    if let Some(name) = agent_name {
        let _ = app.emit("select-agent", name);
    }
}

#[tauri::command]
fn toggle_popover(app: AppHandle) {
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
    // Try to position below the tray icon (macOS) or above it (Windows/Linux).
    // Fall back to top-right corner of the primary monitor.
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

// ─── Background event bridge ───────────────────────────────────────────────

fn spawn_agent_watcher(app: AppHandle, mut rx: tokio::sync::watch::Receiver<Vec<AgentEntry>>) {
    tokio::spawn(async move {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            let entries = rx.borrow().clone();
            // Update managed state and broadcast to all windows.
            if let Some(state) = app.try_state::<AgentState>() {
                *state.0.lock().unwrap() = entries.clone();
            }
            let _ = app.emit("agents-updated", &entries);
        }
    });
}

// ─── App bootstrap ─────────────────────────────────────────────────────────

pub fn run() {
    init_tracing();
    tracing::info!(
        version = mur_gui_core::CRATE_VERSION,
        "starting mur-hub-gui"
    );

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(AgentState(Mutex::new(Vec::new())))
        .setup(|app| {
            // Start agent discovery.
            let mur_home = mur_home_path();
            let (discovery, rx) = AgentDiscovery::new(mur_home);
            // Pre-populate state with initial scan.
            {
                let initial = rx.borrow().clone();
                *app.state::<AgentState>().0.lock().unwrap() = initial;
            }
            discovery.run();
            spawn_agent_watcher(app.handle().clone(), rx);

            // Register global shortcut CmdOrCtrl+Shift+M → toggle_popover.
            let handle = app.handle().clone();
            app.global_shortcut().on_shortcut(
                Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyM),
                move |_app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_popover(handle.clone());
                    }
                },
            )?;

            // Build tray.
            let open_item = MenuItem::with_id(app, "open", "Open Hub", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => open_dashboard(app.clone(), None),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_popover(tray.app_handle().clone());
                    }
                })
                .build(app)?;

            // Wire dashboard close → hide (not quit).
            if let Some(win) = app.get_webview_window("dashboard") {
                let win_clone = win.clone();
                win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = win_clone.hide();
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_agents,
            open_dashboard,
            toggle_popover,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn mur_home_path() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".mur")
        })
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();
}

#[cfg(test)]
mod tests {
    #[test]
    fn lib_links() {
        let _ = super::init_tracing;
        let _ = super::mur_home_path;
    }
}
