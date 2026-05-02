// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bootstrap;
mod commands;
mod companion_bridge;
mod multimodal;
mod sidecar;
mod theme;
mod voice;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_tracing();

    info!("starting mur-agent-gui");

    use std::sync::Arc;
    let sidecar_mgr = Arc::new(sidecar::SidecarManager::new());

    tauri::Builder::default()
        .manage(sidecar_mgr.clone())
        .manage(companion_bridge::state::BridgeState::default())
        .setup(move |app| {
            use tauri::Manager;
            use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
            use tauri::tray::TrayIconBuilder;

            // SIGTERM/SIGINT proxy — when the GUI is killed externally
            // (kill, launchd shutdown, parent process death), make sure
            // the runtime sidecar gets a clean SIGTERM so it flushes
            // telemetry and drops running.lock before we exit. Routes
            // exit through `app.exit(0)` so Tauri tears down windows
            // and runs plugin on_exit hooks (vs. std::process::exit
            // which bypasses them).
            #[cfg(unix)]
            {
                let app_handle = app.handle().clone();
                let mgr = sidecar_mgr.clone();
                tauri::async_runtime::spawn(async move {
                    use tokio::signal::unix::{SignalKind, signal};
                    let (mut term, mut int) = match (
                        signal(SignalKind::terminate()),
                        signal(SignalKind::interrupt()),
                    ) {
                        (Ok(t), Ok(i)) => (t, i),
                        _ => {
                            tracing::warn!("install tokio signal handlers failed");
                            return;
                        }
                    };
                    tokio::select! {
                        _ = term.recv() => tracing::info!("SIGTERM received"),
                        _ = int.recv()  => tracing::info!("SIGINT received"),
                    }
                    let _ = mgr.stop();
                    app_handle.exit(0);
                });
            }

            // First-launch payload extraction + identity mint.
            let resource_dir = app
                .path()
                .resource_dir()
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
            match bootstrap::bootstrap_if_needed(&resource_dir) {
                Ok(meta) => {
                    info!(
                        agent = %meta.agent_name,
                        mode = ?meta.mode,
                        theme = %meta.theme_default,
                        "bootstrap ok"
                    );
                    // SAFETY: setup runs before any Tauri command; single-thread.
                    unsafe {
                        std::env::set_var("MUR_GUI_AGENT_NAME", &meta.agent_name);
                        std::env::set_var("MUR_GUI_THEME_DEFAULT", &meta.theme_default);
                    }

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

            // Voice subsystem (D1 / M1) — default-off opt-in.
            // Register VoiceManager state up-front so the voice_*
            // commands can read it. Boot-time hotkey re-registration
            // handles the case where the user enabled voice in a
            // previous session: voice_state.json says enabled=true,
            // so we re-register the PTT hotkey here without re-running
            // the full voice_enable flow.
            let app_data = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("mur-agent-gui"));
            let app_data_for_hotkey = app_data.clone();
            let voice_app_handle = app.handle().clone();
            tauri::async_runtime::block_on(async move {
                match voice::VoiceManager::new(app_data).await {
                    Ok(mgr) => {
                        let was_enabled = mgr.is_enabled();
                        let state: commands::VoiceManagerState =
                            std::sync::Arc::new(tokio::sync::RwLock::new(mgr));
                        voice_app_handle.manage(state);
                        voice_app_handle
                            .manage(std::sync::Arc::new(commands::ActiveCapture::default()));
                        if was_enabled {
                            // Re-register the persisted hotkey (falls
                            // back to default if hotkey.json absent).
                            let cfg = voice::hotkey::load_hotkey(&app_data_for_hotkey).await;
                            match cfg.to_shortcut() {
                                Ok(s) => {
                                    if let Err(e) = voice::hotkey::register_shortcut(
                                        &voice_app_handle,
                                        &s,
                                    ) {
                                        tracing::warn!("re-register PTT on boot: {e:#}");
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "persisted hotkey config invalid ({e}); falling back to default"
                                    );
                                    if let Err(e2) = voice::hotkey::register_ptt(
                                        &voice_app_handle,
                                    ) {
                                        tracing::warn!("re-register default PTT on boot: {e2:#}");
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("voice subsystem init failed: {e:#}");
                    }
                }
            });

            // Tray menu — primary UX for the menubar launcher.
            let show_settings =
                MenuItem::with_id(app, "show-settings", "Show Settings…", true, None::<&str>)?;
            let show_logs = MenuItem::with_id(app, "show-logs", "Show Logs…", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let start = MenuItem::with_id(app, "start", "Start Agent", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "Stop Agent", true, None::<&str>)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &show_settings,
                    &show_logs,
                    &sep1,
                    &start,
                    &stop,
                    &sep2,
                    &quit,
                ],
            )?;

            // Load the tray icon from the active theme's resource dir.
            // tauri.conf.json's `trayIcon.iconPath` is ignored when we
            // build the tray manually, so we resolve it ourselves here.
            let theme_name =
                std::env::var("MUR_GUI_THEME_DEFAULT").unwrap_or_else(|_| "light".to_string());
            let themes_root = theme::resolve_themes_root(Some(&resource_dir));
            let tray_icon_path = themes_root.join(&theme_name).join("tray-template.png");
            let tray_icon = match tauri::image::Image::from_path(&tray_icon_path) {
                Ok(img) => img,
                Err(e) => {
                    tracing::warn!(
                        "tray icon load failed at {}: {e} — using bundle icon fallback",
                        tray_icon_path.display()
                    );
                    app.default_window_icon().cloned().unwrap_or_else(|| {
                        tauri::image::Image::new_owned(vec![0u8; 16 * 16 * 4], 16, 16)
                    })
                }
            };

            let mgr_for_menu = sidecar_mgr.clone();
            let _tray = TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(true)
                .icon(tray_icon)
                .icon_as_template(true)
                .on_menu_event(move |app, event| {
                    let agent_name = std::env::var("MUR_GUI_AGENT_NAME")
                        .unwrap_or_else(|_| "template".to_string());
                    match event.id.as_ref() {
                        "show-settings" => {
                            if let Some(window) = app.get_webview_window("settings") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "show-logs" => {
                            if let Some(window) = app.get_webview_window("logs") {
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
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        // ── D3 / M3.6.3 — drag-drop event → React ──────────────────
        //
        // Tauri 2 delivers drag-drop through `WindowEvent::DragDrop`.
        // The webview ALSO fires HTML5 `drop` events on its own, so
        // we use `DropDeduper` (sorted-paths within a 120ms window —
        // workaround for Tauri issue #14134, which also covers the
        // double-fire seen on macOS) to make sure the React side gets
        // exactly one `multimodal://drop` per physical drop. React
        // then invokes `multimodal_drop` with these paths via the
        // Tauri RPC.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
                use std::sync::Mutex;
                use tauri::Emitter;
                static DEDUPE: Mutex<Option<crate::multimodal::DropDeduper>> = Mutex::new(None);
                let now = std::time::Instant::now();
                let mut g = DEDUPE.lock().unwrap_or_else(|p| p.into_inner());
                if g.is_none() {
                    *g = Some(crate::multimodal::DropDeduper::new());
                }
                if !g.as_mut().unwrap().observe(paths, now) {
                    return;
                }
                drop(g);
                if let Err(e) = window.emit("multimodal://drop", paths.clone()) {
                    tracing::warn!("emit multimodal://drop: {e:#}");
                }
            }
        })
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
            commands::get_default_theme,
            // Model registry (PR-5)
            commands::list_models,
            commands::get_active_model_ref,
            commands::set_active_model_ref,
            commands::set_secret,
            // Voice (D1 / M1) — default-off, opt-in via voice_enable
            commands::voice_status,
            commands::voice_enable,
            commands::voice_disable,
            commands::voice_list_installed,
            commands::voice_set_default,
            commands::voice_download,
            commands::voice_stt_status,
            commands::voice_stt_download,
            commands::tts_speak,
            commands::stt_transcribe_pcm16k,
            commands::voice_start_capture,
            commands::voice_stop_capture,
            commands::voice_get_hotkey,
            commands::voice_rebind_hotkey,
            // D2 onboarding (M2.4)
            commands::companion_onboarding_status,
            commands::companion_onboarding_submit,
            commands::companion_onboarding_skip,
            // D3 multimodal drag-drop / paste (M3.6)
            commands::multimodal_drop,
            commands::multimodal_paste,
            // D5 companion → GUI bridge (M5.3)
            companion_bridge::commands::companion_bridge_pending,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    let log_dir = log_dir().unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::never(&log_dir, "gui.log");
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // Tee to stderr too: if the file writer fails (disk full, perm
    // denied, container without home dir), the operator still sees
    // logs in the terminal that launched the .app for debugging.
    let writer = file_appender.and(std::io::stderr);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
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
