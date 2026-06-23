//! Skills / MCP install & remove — Hub side. Thin wrappers around the
//! `mur agent skill ...` / `mur agent mcp ...` CLI code paths so the GUI
//! and CLI share one implementation (validation, binary pinning,
//! entitlement allow-listing). Each mutation returns the refreshed
//! `AgentDetail` so the panel re-renders without a second round-trip.

use crate::detail::{AgentDetail, get_agent_detail};
use mur_core::cmd::agent::mcp::{McpAddPin, cmd_mcp_add, cmd_mcp_remove, cmd_mcp_set_enabled};
use mur_core::cmd::agent::skill::{cmd_skill_add, cmd_skill_remove, cmd_skill_set_enabled};
use serde::Serialize;

/// Result of a skill install: the refreshed agent detail plus the id under
/// which the skill was registered, so the UI can report the real outcome
/// (the installed skill's name) instead of a blanket "installed" message.
#[derive(Debug, Clone, Serialize)]
pub struct SkillInstallResult {
    pub detail: AgentDetail,
    /// The canonical id the skill was registered as, e.g. `skills/foo.yaml`.
    pub installed_id: String,
}

#[tauri::command]
pub fn agent_skill_install(
    name: String,
    source_path: String,
) -> Result<SkillInstallResult, String> {
    cmd_skill_add(&name, &source_path).map_err(|e| format!("{e:#}"))?;
    // Mirror `cmd_skill_add`'s id derivation: the source basename, registered
    // under `skills/<basename>`. Computed only after a successful add.
    let installed_id = std::path::Path::new(&source_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|b| format!("skills/{b}"))
        .unwrap_or_else(|| source_path.clone());
    let detail = get_agent_detail(name)?;
    Ok(SkillInstallResult {
        detail,
        installed_id,
    })
}

#[tauri::command]
pub fn agent_skill_uninstall(name: String, skill_id: String) -> Result<AgentDetail, String> {
    cmd_skill_remove(&name, &skill_id).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_mcp_add(
    name: String,
    server_id: String,
    command: String,
    args: Vec<String>,
) -> Result<AgentDetail, String> {
    let server_id = server_id.trim();
    let command = command.trim();
    if server_id.is_empty() {
        return Err("server id must not be empty".into());
    }
    if command.is_empty() {
        return Err("command must not be empty".into());
    }
    // force=true: the GUI itself is the confirmation step (explicit form
    // submit), so skip the CLI's interactive y/N prompt.
    cmd_mcp_add(
        &name,
        server_id,
        command,
        &args,
        McpAddPin {
            force: true,
            ..Default::default()
        },
    )
    .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_mcp_remove(name: String, server_id: String) -> Result<AgentDetail, String> {
    cmd_mcp_remove(&name, &server_id).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

/// Non-destructive enable/disable of an installed skill (Phase-1 denylist).
#[tauri::command]
pub fn agent_skill_toggle(
    name: String,
    skill_id: String,
    enabled: bool,
) -> Result<AgentDetail, String> {
    cmd_skill_set_enabled(&name, &skill_id, enabled).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

/// Non-destructive enable/disable of a configured MCP server (Phase-1 denylist).
#[tauri::command]
pub fn agent_mcp_toggle(
    name: String,
    server_id: String,
    enabled: bool,
) -> Result<AgentDetail, String> {
    cmd_mcp_set_enabled(&name, &server_id, enabled).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}
