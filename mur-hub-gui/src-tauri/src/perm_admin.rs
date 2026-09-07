//! Permission writes from the Hub (spec 2026-09-07 §P2). Every command is one
//! `cmd_perm_*` call and a re-read: the CLI function loads the whole profile,
//! changes one field, validates with its own rules, and saves the whole
//! profile — so nothing here can drop a field the DTO does not model (#957),
//! and the error text the CLI would print is what the UI shows.

use crate::detail::{AgentDetail, get_agent_detail};
use mur_common::agent::ToolPolicy;
use mur_core::cmd::agent::{
    cmd_perm_allow_host, cmd_perm_allow_read, cmd_perm_allow_spawn, cmd_perm_allow_spawn_dir,
    cmd_perm_allow_write, cmd_perm_clear_tool, cmd_perm_deny_host, cmd_perm_deny_path,
    cmd_perm_deny_spawn, cmd_perm_deny_spawn_dir, cmd_perm_remove_path, cmd_perm_set_mode,
    cmd_perm_set_tool,
};

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tauri::command]
pub fn agent_perm_set_outbound_mode(name: String, mode: String) -> Result<AgentDetail, String> {
    cmd_perm_set_mode(&name, "network.outbound", &mode).map_err(err)?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_perm_allow_host(name: String, host: String) -> Result<AgentDetail, String> {
    cmd_perm_allow_host(&name, host.trim()).map_err(err)?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_perm_deny_host(name: String, host: String) -> Result<AgentDetail, String> {
    cmd_perm_deny_host(&name, &host).map_err(err)?;
    get_agent_detail(name)
}

/// `verb` is the list name the P1 view uses: read | write | deny.
#[tauri::command]
pub fn agent_perm_grant_path(
    name: String,
    verb: String,
    path: String,
) -> Result<AgentDetail, String> {
    let grant = grant_for(&verb)?;
    grant(&name, &path).map_err(err)?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_perm_remove_path(
    name: String,
    verb: String,
    path: String,
) -> Result<AgentDetail, String> {
    cmd_perm_remove_path(&name, &verb, &path).map_err(err)?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_perm_set_spawn_mode(name: String, mode: String) -> Result<AgentDetail, String> {
    cmd_perm_set_mode(&name, "processes.spawn", &mode).map_err(err)?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_perm_allow_spawn(name: String, program: String) -> Result<AgentDetail, String> {
    cmd_perm_allow_spawn(&name, program.trim()).map_err(err)?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_perm_deny_spawn(name: String, program: String) -> Result<AgentDetail, String> {
    cmd_perm_deny_spawn(&name, &program).map_err(err)?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_perm_allow_spawn_dir(name: String, dir: String) -> Result<AgentDetail, String> {
    cmd_perm_allow_spawn_dir(&name, dir.trim()).map_err(err)?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_perm_deny_spawn_dir(name: String, dir: String) -> Result<AgentDetail, String> {
    cmd_perm_deny_spawn_dir(&name, &dir).map_err(err)?;
    get_agent_detail(name)
}

/// `policy` arrives as the serde name (`allow` / `ask` / `deny`), which is
/// also what the P1 view emits — no second spelling to keep in step.
#[tauri::command]
pub fn agent_perm_set_tool(
    name: String,
    pattern: String,
    policy: ToolPolicy,
) -> Result<AgentDetail, String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Err("tool pattern must not be empty".into());
    }
    cmd_perm_set_tool(&name, policy, pattern).map_err(err)?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_perm_clear_tool(name: String, pattern: String) -> Result<AgentDetail, String> {
    cmd_perm_clear_tool(&name, &pattern).map_err(err)?;
    get_agent_detail(name)
}

type Grant = fn(&str, &str) -> anyhow::Result<()>;

fn grant_for(verb: &str) -> Result<Grant, String> {
    Ok(match verb {
        "read" => cmd_perm_allow_read,
        "write" => cmd_perm_allow_write,
        "deny" => cmd_perm_deny_path,
        other => return Err(format!("unknown grant list '{other}' (read, write, deny)")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three list names the view emits map to the three CLI grants;
    /// anything else is refused before any profile is touched.
    #[test]
    fn grant_verbs_are_the_view_list_names() {
        for v in ["read", "write", "deny"] {
            assert!(grant_for(v).is_ok(), "{v}");
        }
        assert!(grant_for("exec").is_err());
        assert!(grant_for("").is_err());
    }
}
