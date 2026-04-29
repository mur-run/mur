//! MCP server admin operations for a single agent.

use anyhow::Result;
use mur_common::agent::McpServerEntry;

use crate::cmd::agent;

// ─── mutators ─────────────────────────────────────────────────────

pub fn add(name: &str, server_id: &str, command: &str, args: &[String]) -> Result<()> {
    agent::cmd_mcp_add(name, server_id, command, args)
}

pub fn remove(name: &str, server_id: &str) -> Result<()> {
    agent::cmd_mcp_remove(name, server_id)
}

pub fn rename(name: &str, old: &str, new: &str) -> Result<()> {
    agent::cmd_mcp_rename(name, old, new)
}

// ─── queries ──────────────────────────────────────────────────────

/// List MCP servers as typed entries (no stdout).
pub fn list(name: &str) -> Result<Vec<McpServerEntry>> {
    let (_path, profile) = agent::load_profile_for_edit(name)?;
    Ok(profile.mcp_servers)
}
