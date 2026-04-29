//! Tauri command surface — typed RPC between the React frontend and
//! the embedded `mur-core` admin library.
//!
//! Each command is a thin wrapper over `mur_core::agent_admin::*`.
//! Mutators return `Result<(), String>` (Tauri serialises String errors
//! cleanly to the webview); queries return typed values that derive
//! `Serialize`.
//!
//! The agent name is read from `AGENT_NAME` env (set by the bootstrap
//! at first launch when the agent payload is extracted to
//! `~/.mur/agents/<name>/`). Stub here; real wiring lands in P1.3 +
//! P1.6.

use mur_common::agent::{Entitlements, McpServerEntry};
use mur_core::agent_admin;
use serde::Serialize;

fn agent_name() -> String {
    std::env::var("MUR_GUI_AGENT_NAME").unwrap_or_else(|_| "template".to_string())
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ─── Status / lifecycle ────────────────────────────────────────────

#[tauri::command]
pub fn status() -> Result<agent_admin::lifecycle::StatusView, String> {
    agent_admin::lifecycle::status(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn start_agent(
    app: tauri::AppHandle,
    mgr: tauri::State<'_, std::sync::Arc<crate::sidecar::SidecarManager>>,
) -> Result<(), String> {
    mgr.start(&app, &agent_name()).map_err(err)
}

#[tauri::command]
pub fn stop_agent(
    mgr: tauri::State<'_, std::sync::Arc<crate::sidecar::SidecarManager>>,
) -> Result<(), String> {
    mgr.stop().map_err(err)?;
    // Also call the agent_admin stop for consistency (it uses
    // running.lock to send SIGTERM in case the sidecar mgr is out
    // of sync with the on-disk state).
    let _ = agent_admin::lifecycle::stop(&agent_name());
    Ok(())
}

#[tauri::command]
pub fn restart_agent(
    app: tauri::AppHandle,
    mgr: tauri::State<'_, std::sync::Arc<crate::sidecar::SidecarManager>>,
) -> Result<(), String> {
    mgr.stop().map_err(err)?;
    mgr.start(&app, &agent_name()).map_err(err)
}

// ─── System Prompt ─────────────────────────────────────────────────

#[tauri::command]
pub fn prompt_get() -> Result<String, String> {
    agent_admin::prompt::get(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn prompt_set(content: String) -> Result<(), String> {
    agent_admin::prompt::set(&agent_name(), Some(&content), None).map_err(err)
}

// ─── Skills ────────────────────────────────────────────────────────

#[tauri::command]
pub fn skill_list() -> Result<Vec<String>, String> {
    agent_admin::skill::list(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn skill_show(query: String) -> Result<String, String> {
    agent_admin::skill::show(&agent_name(), &query).map_err(err)
}

#[tauri::command]
pub fn skill_add(source: String) -> Result<(), String> {
    agent_admin::skill::add(&agent_name(), &source).map_err(err)
}

#[tauri::command]
pub fn skill_remove(query: String) -> Result<(), String> {
    agent_admin::skill::remove(&agent_name(), &query).map_err(err)
}

// ─── MCP Servers ───────────────────────────────────────────────────

#[tauri::command]
pub fn mcp_list() -> Result<Vec<McpServerEntry>, String> {
    agent_admin::mcp::list(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn mcp_add(server_id: String, command: String, args: Vec<String>) -> Result<(), String> {
    agent_admin::mcp::add(&agent_name(), &server_id, &command, &args).map_err(err)
}

#[tauri::command]
pub fn mcp_remove(server_id: String) -> Result<(), String> {
    agent_admin::mcp::remove(&agent_name(), &server_id).map_err(err)
}

#[tauri::command]
pub fn mcp_rename(old: String, new: String) -> Result<(), String> {
    agent_admin::mcp::rename(&agent_name(), &old, &new).map_err(err)
}

// ─── Permissions ───────────────────────────────────────────────────

#[tauri::command]
pub fn perm_view() -> Result<Entitlements, String> {
    agent_admin::perm::view(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn perm_set_mode(key: String, value: String) -> Result<(), String> {
    agent_admin::perm::set_mode(&agent_name(), &key, &value).map_err(err)
}

#[tauri::command]
pub fn perm_allow_host(glob: String) -> Result<(), String> {
    agent_admin::perm::allow_host(&agent_name(), &glob).map_err(err)
}

#[tauri::command]
pub fn perm_deny_host(glob: String) -> Result<(), String> {
    agent_admin::perm::deny_host(&agent_name(), &glob).map_err(err)
}

#[tauri::command]
pub fn perm_allow_read(path: String) -> Result<(), String> {
    agent_admin::perm::allow_read(&agent_name(), &path).map_err(err)
}

#[tauri::command]
pub fn perm_allow_write(path: String) -> Result<(), String> {
    agent_admin::perm::allow_write(&agent_name(), &path).map_err(err)
}

#[tauri::command]
pub fn perm_deny_path(path: String) -> Result<(), String> {
    agent_admin::perm::deny_path(&agent_name(), &path).map_err(err)
}

#[tauri::command]
pub fn perm_allow_spawn(binary: String) -> Result<(), String> {
    agent_admin::perm::allow_spawn(&agent_name(), &binary).map_err(err)
}

#[tauri::command]
pub fn perm_deny_spawn(binary: String) -> Result<(), String> {
    agent_admin::perm::deny_spawn(&agent_name(), &binary).map_err(err)
}

#[tauri::command]
pub fn perm_set_limit(key: String, value: u64) -> Result<(), String> {
    agent_admin::perm::set_limit(&agent_name(), &key, value).map_err(err)
}

// ─── Observability ─────────────────────────────────────────────────

#[tauri::command]
pub fn stats() -> Result<agent_admin::observability::StatsView, String> {
    agent_admin::observability::stats(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn logs(tail: usize) -> Result<String, String> {
    agent_admin::observability::logs(&agent_name(), tail).map_err(err)
}

// ─── Theme ─────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ThemeInfo {
    pub name: String,
    pub display_name: String,
    pub kind: String,
}

#[tauri::command]
pub fn list_themes() -> Result<Vec<ThemeInfo>, String> {
    crate::theme::list().map_err(err)
}

#[tauri::command]
pub fn set_theme(name: String) -> Result<(), String> {
    crate::theme::activate(&name).map_err(err)
}
