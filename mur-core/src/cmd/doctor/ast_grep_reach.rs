//! #1984: ast-grep is installed under mur home but this agent cannot use it.
//!
//! Offline and read-only, like the rest of `agent_doctor`: it compares the
//! profile with what `mur code-nav setup` wires (see
//! `code_nav::ast_grep_wire`), and names the one command that fixes it.
//! Silent when the pinned binary is not installed — then there is nothing
//! the agent could reach.

use std::path::Path;

use mur_common::AgentProfile;

use super::Check;
use crate::cmd::code_nav::ast_grep_wire::{is_mur_mcp_server, required_paths};

pub(super) const CHECK_NAME: &str = "ast-grep";

/// `None` when ast-grep is not installed; otherwise one check, failing with
/// every missing piece listed.
pub(super) fn check(mur_home: &Path, agent: &str, profile: &AgentProfile) -> Option<Check> {
    let (bin, run_dir) = required_paths(mur_home);
    if !bin.is_file() {
        return None;
    }
    let missing = gaps(profile, &bin, &run_dir);
    if missing.is_empty() {
        return Some(Check::new(
            CHECK_NAME,
            true,
            format!("installed at {} and wired to this agent", bin.display()),
        ));
    }
    Some(Check::new(
        CHECK_NAME,
        false,
        format!(
            "installed at {} but unreachable by this agent: {}. Fix: mur code-nav setup \
             --agent {agent}, then restart the agent",
            bin.display(),
            missing.join("; ")
        ),
    ))
}

/// What a wired agent has and this one lacks. Pure, for the tests.
pub(super) fn gaps(profile: &AgentProfile, bin: &Path, run_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    match profile
        .mcp_servers
        .iter()
        .find(|e| is_mur_mcp_server(&e.command))
    {
        None => out.push("no MUR MCP server entry (ast_grep_search is never offered)".into()),
        Some(e) if !profile.mcp_enabled(&e.name) => {
            out.push(format!("MCP server `{}` is disabled", e.name))
        }
        Some(_) => {}
    }
    let bin_s = bin.display().to_string();
    if !profile
        .entitlements
        .processes
        .spawn
        .allowed
        .contains(&bin_s)
    {
        out.push(format!("{bin_s} is not in the spawn allowlist"));
    }
    let dir_s = run_dir.display().to_string();
    let fs = &profile.entitlements.filesystem;
    if !(fs.read.contains(&dir_s) && fs.write.contains(&dir_s)) {
        out.push(format!("no read+write grant on {dir_s}"));
    }
    out
}
