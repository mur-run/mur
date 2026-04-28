// Prevents additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod theme;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_tracing();

    info!("starting mur-agent-gui");

    tauri::Builder::default()
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
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(file_appender)
        .init();
}

#[cfg(target_os = "macos")]
fn log_dir() -> Option<std::path::PathBuf> {
    dirs_next::home_dir().map(|h| h.join("Library/Logs/MurAgent"))
}

#[cfg(target_os = "linux")]
fn log_dir() -> Option<std::path::PathBuf> {
    dirs_next::data_local_dir().map(|d| d.join("MurAgent/logs"))
}

#[cfg(target_os = "windows")]
fn log_dir() -> Option<std::path::PathBuf> {
    dirs_next::data_local_dir().map(|d| d.join("MurAgent/Logs"))
}
