// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bootstrap;
mod commands;
mod sidecar;
mod theme;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_tracing();

    info!("starting mur-agent-gui");

    use std::sync::Arc;
    let sidecar_mgr = Arc::new(sidecar::SidecarManager::new());

    // SIGTERM/SIGINT proxy — when the GUI is killed externally
    // (kill, launchd shutdown, parent process death), make sure the
    // runtime sidecar gets a clean SIGTERM so it flushes telemetry
    // and drops running.lock before we exit. The tray Quit menu has
    // its own handler (in setup) that does the same; this covers
    // the SIGTERM-from-outside path.
    #[cfg(unix)]
    {
        let mgr = sidecar_mgr.clone();
        std::thread::spawn(move || {
            // SAFETY: signal(7) handlers must not allocate or take
            // locks. We only set a flag-like Mutex<bool> via
            // SidecarManager::stop, which uses parking_lot-style
            // logic — close enough for the "process is dying anyway"
            // path. If this proves problematic, swap to libc::signal
            // with a global atomic flag and check it from a tokio
            // task instead.
            let mut signals = match signal_hook::iterator::Signals::new([
                signal_hook::consts::SIGTERM,
                signal_hook::consts::SIGINT,
            ]) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("install signal handler: {e}");
                    return;
                }
            };
            for _sig in signals.forever() {
                tracing::info!("received SIGTERM/SIGINT, stopping sidecar");
                let _ = mgr.stop();
                std::thread::sleep(std::time::Duration::from_millis(500));
                std::process::exit(0);
            }
        });
    }

    tauri::Builder::default()
        .manage(sidecar_mgr.clone())
        .setup(move |app| {
            use tauri::Manager;
            use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
            use tauri::tray::TrayIconBuilder;

            // First-launch payload extraction + identity mint.
            let resource_dir = app
                .path()
                .resource_dir()
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
            match bootstrap::bootstrap_if_needed(&resource_dir) {
                Ok(meta) => {
                    info!(
                        "bootstrap ok: agent='{}', mode={:?}, theme='{}'",
                        meta.agent_name, meta.mode, meta.theme_default
                    );
                    // SAFETY: setup runs before any Tauri command; single-thread.
                    unsafe { std::env::set_var("MUR_GUI_AGENT_NAME", &meta.agent_name); }

                    // Auto-spawn the sidecar so "click app → agent runs"
                    // is the default flow. User can stop/restart from
                    // the tray menu or Status tab.
                    let app_handle = app.handle().clone();
                    let mgr = sidecar_mgr.clone();
                    let agent_name = meta.agent_name.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = mgr.start(&app_handle, &agent_name) {
                            tracing::error!("auto-start sidecar failed: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("bootstrap failed: {e:#}");
                }
            }

            // Tray menu — primary UX for the menubar launcher.
            let show_settings = MenuItem::with_id(app, "show-settings", "Show Settings…", true, None::<&str>)?;
            let show_logs = MenuItem::with_id(app, "show-logs", "Show Logs…", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let start = MenuItem::with_id(app, "start", "Start Agent", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "Stop Agent", true, None::<&str>)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_settings, &show_logs, &sep1, &start, &stop, &sep2, &quit])?;

            let mgr_for_menu = sidecar_mgr.clone();
            let _tray = TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(true)
                .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
                    // Fallback to a transparent 16x16 if Tauri couldn't
                    // resolve the bundle icon (e.g. dev runs).
                    tauri::image::Image::new_owned(vec![0u8; 16 * 16 * 4], 16, 16)
                }))
                .on_menu_event(move |app, event| {
                    let agent_name = std::env::var("MUR_GUI_AGENT_NAME").unwrap_or_else(|_| "template".to_string());
                    match event.id.as_ref() {
                        "show-settings" => {
                            if let Some(window) = app.get_webview_window("settings") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "show-logs" => {
                            if let Some(window) = app.get_webview_window("settings") {
                                // P1.5 follow-up: open a dedicated logs window.
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "start" => {
                            let app_clone = app.clone();
                            let mgr = mgr_for_menu.clone();
                            let name = agent_name.clone();
                            tauri::async_runtime::spawn(async move {
                                if let Err(e) = mgr.start(&app_clone, &name) {
                                    tracing::error!("tray Start: {e:#}");
                                }
                            });
                        }
                        "stop" => {
                            if let Err(e) = mgr_for_menu.stop() {
                                tracing::error!("tray Stop: {e:#}");
                            }
                        }
                        "quit" => {
                            // Stop sidecar before quitting so the runtime gets
                            // a clean SIGTERM and drops running.lock.
                            let _ = mgr_for_menu.stop();
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            Ok(())
        })
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            // Status / lifecycle
            commands::status,
            commands::start_agent,
            commands::stop_agent,
            commands::restart_agent,
            // System Prompt
            commands::prompt_get,
            commands::prompt_set,
            // Skills
            commands::skill_list,
            commands::skill_show,
            commands::skill_add,
            commands::skill_remove,
            // MCP servers
            commands::mcp_list,
            commands::mcp_add,
            commands::mcp_remove,
            commands::mcp_rename,
            // Permissions
            commands::perm_view,
            commands::perm_set_mode,
            commands::perm_allow_host,
            commands::perm_deny_host,
            commands::perm_allow_read,
            commands::perm_allow_write,
            commands::perm_deny_path,
            commands::perm_allow_spawn,
            commands::perm_deny_spawn,
            commands::perm_set_limit,
            // Observability
            commands::stats,
            commands::logs,
            // Theme
            commands::list_themes,
            commands::set_theme,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    Ok(())
}

fn init_tracing() {
    let log_dir = log_dir().unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::never(&log_dir, "gui.log");
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file_appender)
        .with_ansi(false)
        .init();
}

#[cfg(target_os = "macos")]
fn log_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/Logs/MurAgent"))
}

#[cfg(target_os = "linux")]
fn log_dir() -> Option<std::path::PathBuf> {
    dirs::data_local_dir().map(|d| d.join("MurAgent/logs"))
}

#[cfg(target_os = "windows")]
fn log_dir() -> Option<std::path::PathBuf> {
    dirs::data_local_dir().map(|d| d.join("MurAgent/Logs"))
}
