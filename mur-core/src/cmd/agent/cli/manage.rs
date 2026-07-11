//! TUI-safe agent management for `/mcp` and `/skill` slash commands.
//!
//! The `mur agent mcp|skill` CLI handlers print to stdout and (for installs)
//! prompt on stdin — both unusable inside a raw-mode alternate screen. These
//! variants return the text to render and never touch the terminal; the
//! silent CLI helpers (`cmd_mcp_remove`, `cmd_skill_add`, `cmd_skill_remove`)
//! are reused as-is.

use anyhow::{Result, bail};
use mur_common::agent::McpServerEntry;

use crate::cmd::agent::{load_profile_for_edit, save_profile};

/// Reminder appended after any profile mutation: the supervisor only reads
/// the profile at startup.
pub const RESTART_HINT: &str =
    "profile updated — restart the agent to apply (mur agent stop <name>, then start it again)";

pub fn mcp_list(agent: &str) -> Result<String> {
    let (_path, profile) = load_profile_for_edit(agent)?;
    if profile.mcp_servers.is_empty() {
        return Ok("(no MCP servers configured)".into());
    }
    let mut out = String::from("MCP servers:\n");
    for s in &profile.mcp_servers {
        let pinned = if s.binary_sha256.is_some() {
            " (pinned)"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {} — {} {}{}\n",
            s.name,
            s.command,
            s.args.join(" "),
            pinned
        ));
    }
    Ok(out.trim_end().to_string())
}

/// Non-interactive port of `cmd_mcp_add` (force semantics): best-effort
/// binary pin, spawn-allowlist sync, warnings folded into the returned text.
pub fn mcp_add(agent: &str, server_id: &str, command: &str, args: &[String]) -> Result<String> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    if profile.mcp_servers.iter().any(|s| s.name == server_id) {
        bail!("MCP server '{server_id}' already exists on '{agent}'");
    }

    let mut notes = Vec::new();
    let binary_sha256 = match crate::cmd::agent_mcp_pin::resolve_command(command) {
        Ok(p) => match crate::cmd::agent_mcp_pin::compute_binary_sha256(&p) {
            Ok(h) => {
                notes.push(format!("binary sha256 {}…", &h[..16.min(h.len())]));
                Some(h)
            }
            Err(e) => {
                notes.push(format!(
                    "warning: could not hash {} ({e}); no binary pin",
                    p.display()
                ));
                None
            }
        },
        Err(_) => {
            notes.push(format!(
                "warning: `{command}` not found on PATH; no binary pin"
            ));
            None
        }
    };

    profile.mcp_servers.push(McpServerEntry {
        name: server_id.to_string(),
        command: command.to_string(),
        args: args.to_vec(),
        binary_sha256,
        description_hash: None,
        publisher: None,
        installed_at: Some(chrono::Utc::now()),
        timeout_secs: None,
        network: None,
        url: None,
        auth: None,
        requires_programs: Vec::new(),
    });
    if !profile
        .entitlements
        .processes
        .spawn
        .allowed
        .iter()
        .any(|a| a == command)
    {
        profile
            .entitlements
            .processes
            .spawn
            .allowed
            .push(command.to_string());
    }
    save_profile(&path, &mut profile)?;

    let mut out = format!(
        "added MCP server '{server_id}' ({command} {})",
        args.join(" ")
    );
    for n in notes {
        out.push_str(&format!("\n  {n}"));
    }
    out.push_str(&format!("\n{RESTART_HINT}"));
    Ok(out)
}

pub fn mcp_remove(agent: &str, server_id: &str) -> Result<String> {
    crate::cmd::agent::mcp::cmd_mcp_remove(agent, server_id)?;
    Ok(format!("removed MCP server '{server_id}'\n{RESTART_HINT}"))
}

pub fn skill_list(agent: &str) -> Result<String> {
    let (_path, profile) = load_profile_for_edit(agent)?;
    if profile.skills.is_empty() {
        return Ok("(no skills attached)".into());
    }
    let mut out = String::from("skills:\n");
    for s in &profile.skills {
        out.push_str(&format!("  {s}\n"));
    }
    Ok(out.trim_end().to_string())
}

pub fn skill_add(agent: &str, source: &str) -> Result<String> {
    crate::cmd::agent::skill::cmd_skill_add(agent, source)?;
    Ok(format!("installed skill from '{source}'\n{RESTART_HINT}"))
}

pub fn skill_remove(agent: &str, query: &str) -> Result<String> {
    crate::cmd::agent::skill::cmd_skill_remove(agent, query)?;
    Ok(format!("removed skill '{query}'\n{RESTART_HINT}"))
}

/// Usage strings shown for bad arguments.
pub const MCP_USAGE: &str =
    "usage: /mcp [list] · /mcp add <name> <command> [args…] · /mcp remove <name>";
pub const SKILL_USAGE: &str = "usage: /skill [list] · /skill add <path> (validates + installs a .yaml/.md skill into skills/<name>/skill.yaml) · /skill remove <name>";

/// Dispatch a parsed `/mcp` invocation.
pub fn run_mcp(agent: &str, args: &[String]) -> Result<String> {
    match args.first().map(String::as_str) {
        None | Some("list") => mcp_list(agent),
        Some("add") if args.len() >= 3 => mcp_add(agent, &args[1], &args[2], &args[3..]),
        Some("remove") | Some("rm") if args.len() == 2 => mcp_remove(agent, &args[1]),
        _ => Ok(MCP_USAGE.into()),
    }
}

/// Dispatch a parsed `/skill` invocation.
pub fn run_skill(agent: &str, args: &[String]) -> Result<String> {
    match args.first().map(String::as_str) {
        None | Some("list") => skill_list(agent),
        Some("add") | Some("install") if args.len() == 2 => skill_add(agent, &args[1]),
        Some("remove") | Some("rm") if args.len() == 2 => skill_remove(agent, &args[1]),
        _ => Ok(SKILL_USAGE.into()),
    }
}
