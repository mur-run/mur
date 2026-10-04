//! Task 3.5: the profile's `kind: serena` MCP entry (decision D4).
//!
//! Pure: builds the entry and merges it into an in-memory profile. Saving
//! the profile is task 3.6's apply step, after consent.
//!
//! The entry's shape is the one Layer B ran end to end: `command` is the
//! pinned install's entry point (absolute, never a `PATH` lookup), `args`
//! select the stdio MCP server, and `project` is fixed here, at setup, never
//! inferred from the session cwd. `--project` and both dashboard flags are
//! not written: the runtime appends them at every spawn
//! (`mur_agent_runtime::mcp::serena::launch_args`), so a hand-edited profile
//! cannot drop them.

use anyhow::{Context, Result, bail};
use mur_common::AgentProfile;
use mur_common::agent::{McpServerEntry, McpServerKind};
use std::path::{Path, PathBuf};

use super::plan::SERENA;
use super::serena_install::Record;

/// Entry name. Tool names reach the model as `mcp__serena__<tool>`.
pub const ENTRY_NAME: &str = SERENA;

/// serena's MCP subcommand and transport, as Layer B ran them.
pub const ENTRY_ARGS: [&str; 3] = ["start-mcp-server", "--transport", "stdio"];

/// What [`upsert`] did to the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Updated,
    Unchanged,
}

/// The project serena will serve: canonical, absolute, an existing
/// directory. The same conditions the runtime's startup gate enforces,
/// checked here so setup fails instead of the next agent start.
pub fn checked_project(project: &Path) -> Result<PathBuf> {
    let canon = std::fs::canonicalize(project)
        .with_context(|| format!("project {} does not exist", project.display()))?;
    if !canon.is_dir() {
        bail!("project {} is not a directory", canon.display());
    }
    Ok(canon)
}

/// Build the entry for a verified install. `binary_sha256` pins the entry
/// point the runtime will exec (B0 rule 6), hashed now, at setup.
pub fn build(record: &Record, project: &Path) -> Result<McpServerEntry> {
    let project = checked_project(project)?;
    if !record.bin.is_absolute() {
        bail!(
            "serena entry point {} is not absolute",
            record.bin.display()
        );
    }
    let command = record
        .bin
        .to_str()
        .with_context(|| format!("serena path {} is not UTF-8", record.bin.display()))?
        .to_owned();
    let sha = crate::cmd::agent_mcp_pin::compute_binary_sha256(&record.bin)?;
    Ok(McpServerEntry {
        name: ENTRY_NAME.into(),
        command,
        args: ENTRY_ARGS.iter().map(|a| (*a).to_owned()).collect(),
        binary_sha256: Some(sha),
        installed_at: Some(chrono::Utc::now()),
        kind: Some(McpServerKind::Serena),
        project: Some(project),
        ..Default::default()
    })
}

/// Same entry apart from the install timestamp.
fn same_entry(a: &McpServerEntry, b: &McpServerEntry) -> bool {
    let strip = |e: &McpServerEntry| McpServerEntry {
        installed_at: None,
        ..e.clone()
    };
    strip(a) == strip(b)
}

/// Refuse when `name` is taken by an entry that is not `kind: serena` (the
/// user's own server). Pure, so setup can run it before anything is
/// installed or written, not only at the final profile save.
pub fn check_slot(profile: &AgentProfile, name: &str) -> Result<()> {
    let Some(slot) = profile.mcp_servers.iter().find(|m| m.name == name) else {
        return Ok(());
    };
    if slot.kind != Some(McpServerKind::Serena) {
        bail!(
            "agent `{agent}` already has an MCP server named `{name}` that is not kind: serena; \
             remove or rename it first (`mur agent mcp remove {agent} {name}`)",
            agent = profile.name,
        );
    }
    Ok(())
}

/// Merge `entry` into `profile` and allow its `command` to spawn.
///
/// An existing `kind: serena` entry of the same name is replaced; one that
/// differs only in `installed_at` is left alone, so a re-run reports
/// [`Change::Unchanged`]. A same-named entry that is not `kind: serena` is
/// the user's own server and is refused, never overwritten.
pub fn upsert(profile: &mut AgentProfile, entry: McpServerEntry) -> Result<Change> {
    check_slot(profile, &entry.name)?;
    let spawn = &mut profile.entitlements.processes.spawn.allowed;
    if !spawn.iter().any(|a| a == &entry.command) {
        spawn.push(entry.command.clone());
    }
    let Some(slot) = profile
        .mcp_servers
        .iter_mut()
        .find(|m| m.name == entry.name)
    else {
        profile.mcp_servers.push(entry);
        return Ok(Change::Added);
    };
    if same_entry(slot, &entry) {
        return Ok(Change::Unchanged);
    }
    *slot = entry;
    Ok(Change::Updated)
}

#[cfg(test)]
#[path = "serena_entry_tests.rs"]
mod tests;
