//! MuR Hub — Tauri 2 desktop app.
//!
//! M-h1: tray icon, popover + dashboard windows, agent discovery, global shortcut.
//! M-h2: sidecar Supervisor — spawn/supervise agent runtimes; start_agent/stop_agent commands.
//! M-h4: onboarding wizard — wizard_open/set_persona/.../start_render/finish/cancel.
//! M-h5: pet windows — pet_spawn_at/close/reposition/return_to_hub/list/get_expression.

pub mod companion;
pub mod export_muragent;
pub mod import_muragent;
pub mod mlx_sidecar;
pub mod onboarding;
pub mod pet;
pub mod preset;

use companion::BridgeState;
use mur_gui_core::discovery::{AgentDiscovery, AgentEntry};
use mur_gui_core::event_bus::EventBus;
use mur_gui_core::sidecar::{AgentRuntimeStatus, Supervisor};
use mur_gui_core::stub;
use onboarding::WizardState;
use pet::{EventBusState, PetState};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{
    AppHandle, Emitter, Manager, State,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tracing_subscriber::EnvFilter;

/// Managed Tauri state: current agent metadata snapshot.
pub struct AgentState(pub Mutex<Vec<AgentEntry>>);

/// Managed Tauri state: sidecar supervisor handle.
pub struct SupervisorState(pub Supervisor);

// ─── Tauri commands ────────────────────────────────────────────────────────

#[tauri::command]
fn list_agents(state: State<'_, AgentState>) -> Vec<AgentEntry> {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
async fn start_agent(name: String, supervisor: State<'_, SupervisorState>) -> Result<(), String> {
    supervisor.0.start(&name).await;
    Ok(())
}

#[tauri::command]
async fn stop_agent(name: String, supervisor: State<'_, SupervisorState>) -> Result<(), String> {
    supervisor.0.stop(&name).await;
    Ok(())
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

fn spawn_agent_watcher(app: AppHandle, mut rx: tokio::sync::watch::Receiver<Vec<AgentEntry>>) {
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

fn spawn_runtime_watcher(
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

// ─── App bootstrap ─────────────────────────────────────────────────────────

pub fn run() {
    init_tracing();
    tracing::info!(
        version = mur_gui_core::CRATE_VERSION,
        "starting mur-hub-gui"
    );

    // Handle --regenerate-stub <slug> emitted by mur-agent-launcher when it
    // detects a Hub version mismatch (spec §5.4). Regenerate and exit.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--regenerate-stub")
        && let Some(slug) = args.get(pos + 1)
    {
        let mur_home = mur_home_path();
        let bundle_id = format!("run.mur.agent.{slug}");
        let url_scheme = format!("muragent-{slug}");
        let agent_dir = mur_home.join("agents").join(slug);
        let display_name = load_display_name_for_stub(&agent_dir).unwrap_or_else(|| slug.clone());
        let icon_path = agent_dir.join("icon").join("icon.icns");
        let icon_icns = std::fs::read(&icon_path).ok();
        if let Err(e) = stub::generate(
            slug,
            &display_name,
            &bundle_id,
            &url_scheme,
            icon_icns.as_deref(),
            env!("CARGO_PKG_VERSION"),
            &mur_home,
        ) {
            tracing::error!("--regenerate-stub failed for {slug}: {e}");
            std::process::exit(1);
        }
        std::process::exit(0);
    }

    let mur_home = mur_home_path();
    // Write ~/.mur/host_path so mur-agent-launcher stubs can find the Hub binary
    // and detect version mismatches for self-update (spec §5.2, §5.4).
    if let Err(e) = write_host_path(env!("CARGO_PKG_VERSION"), &mur_home) {
        tracing::warn!("failed to write host_path: {e}");
    }
    let supervisor = Supervisor::new(mur_home.clone());
    let runtime_rx = supervisor.status_receiver();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            for arg in argv.iter() {
                if arg.ends_with(".muragent") {
                    let _ = app.emit("open-muragent-file", arg.clone());
                    break;
                }
            }
            if let Some(win) = app.get_webview_window("dashboard") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .manage(AgentState(Mutex::new(Vec::new())))
        .manage(SupervisorState(supervisor))
        .manage(WizardState(Mutex::new(None)))
        .manage(PetState(Mutex::new(std::collections::HashMap::new())))
        .manage(EventBusState(EventBus::new(256)))
        .manage(BridgeState::default())
        .setup(move |app| {
            // Start agent discovery (filesystem scan).
            let (discovery, agent_rx) = AgentDiscovery::new(mur_home.clone());
            {
                let initial = agent_rx.borrow().clone();
                *app.state::<AgentState>().0.lock().unwrap() = initial;
            }
            discovery.run();
            spawn_agent_watcher(app.handle().clone(), agent_rx);

            // Bridge supervisor status → Tauri events.
            spawn_runtime_watcher(app.handle().clone(), runtime_rx);

            // Scan for stale OS stubs and regenerate in a background task (§5.4).
            stub::scan_and_regenerate_stale(env!("CARGO_PKG_VERSION").into(), mur_home.clone());

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

            #[cfg(target_os = "windows")]
            {
                let args: Vec<String> = std::env::args().collect();
                for arg in args.iter().skip(1) {
                    if arg.ends_with(".muragent") {
                        let _ = app.handle().emit("open-muragent-file", arg.clone());
                        break;
                    }
                }
            }

            // Start the bundled local inference server (best-effort).
            mlx_sidecar::start(app.handle());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_agents,
            start_agent,
            stop_agent,
            open_dashboard,
            toggle_popover,
            onboarding::wizard_open,
            onboarding::wizard_set_persona,
            onboarding::wizard_set_name,
            onboarding::wizard_set_preset,
            onboarding::wizard_set_behavior,
            onboarding::wizard_set_photo,
            onboarding::wizard_start_render,
            onboarding::wizard_finish,
            onboarding::wizard_cancel,
            onboarding::first_launch::check_first_launch,
            onboarding::first_launch::mark_first_launch_done,
            pet::pet_spawn_at,
            pet::pet_close,
            pet::pet_reposition,
            pet::pet_return_to_hub,
            pet::pet_list,
            pet::pet_get_expression,
            pet::hub_emit_event,
            pet::pet_ack_bubble,
            pet::pet_speak,
            preset::import_preset_file,
            preset::import_preset_url,
            import_muragent::inspect_muragent_file,
            import_muragent::install_muragent_file,
            import_muragent::model_resolution_view,
            import_muragent::apply_agent_model,
            export_muragent::export_muragent_file,
            companion::companion_bridge_pending,
            companion::companion_bridge_subscribe,
            companion::companion_bridge_unsubscribe,
            companion::companion_ack,
            companion::companion_unread_count,
            companion::companion_proactive,
            companion::companion_quiet,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = _event {
                for url in urls {
                    if let Ok(path) = url.to_file_path()
                        && let Some(s) = path.to_str()
                    {
                        let _ = _app.emit("open-muragent-file", s.to_string());
                    }
                }
            }
        });
}

pub(crate) fn mur_home_path() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".mur")
        })
}

/// Atomically write `~/.mur/host_path` so per-agent launcher stubs can locate
/// the Hub binary and detect version mismatches (spec §5.2).
///
/// Format: two lines — `<absolute_exe_path>\n<version>`.
fn write_host_path(version: &str, mur_home: &std::path::Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_exe().unwrap());
    std::fs::create_dir_all(mur_home).map_err(|e| anyhow::anyhow!("create mur_home: {e}"))?;
    let tmp = mur_home.join("host_path.tmp");
    std::fs::write(&tmp, format!("{}\n{}", exe.display(), version))
        .map_err(|e| anyhow::anyhow!("write host_path.tmp: {e}"))?;
    std::fs::rename(&tmp, mur_home.join("host_path"))
        .map_err(|e| anyhow::anyhow!("rename host_path: {e}"))?;
    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();
}

fn load_display_name_for_stub(agent_dir: &std::path::Path) -> Option<String> {
    let yaml = std::fs::read_to_string(agent_dir.join("profile.yaml")).ok()?;
    for line in yaml.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            let name = rest.trim().trim_matches('"').trim_matches('\'');
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn lib_links() {
        let _ = super::init_tracing;
        let _ = super::mur_home_path;
    }
}
