//! Entitlement (permission) admin operations for a single agent.
//!
//! Wraps the existing `cmd::agent::cmd_perm_*` functions with a clean
//! library naming convention, plus typed read views for callers that
//! need structured data (e.g. GUI Permissions tab) instead of stdout.

use anyhow::Result;
use mur_common::agent::Entitlements;

use crate::cmd::agent;

// ─── mutators (re-exports under cleaner names) ─────────────────────

pub fn set_mode(name: &str, key: &str, value: &str) -> Result<()> {
    agent::cmd_perm_set_mode(name, key, value)
}

pub fn allow_host(name: &str, glob: &str) -> Result<()> {
    agent::cmd_perm_allow_host(name, glob)
}

pub fn deny_host(name: &str, glob: &str) -> Result<()> {
    agent::cmd_perm_deny_host(name, glob)
}

pub fn allow_read(name: &str, path: &str) -> Result<()> {
    agent::cmd_perm_allow_read(name, path)
}

pub fn allow_write(name: &str, path: &str) -> Result<()> {
    agent::cmd_perm_allow_write(name, path)
}

pub fn deny_path(name: &str, path: &str) -> Result<()> {
    agent::cmd_perm_deny_path(name, path)
}

pub fn allow_spawn(name: &str, binary: &str) -> Result<()> {
    agent::cmd_perm_allow_spawn(name, binary)
}

pub fn deny_spawn(name: &str, binary: &str) -> Result<()> {
    agent::cmd_perm_deny_spawn(name, binary)
}

pub fn set_limit(name: &str, key: &str, value: u64) -> Result<()> {
    agent::cmd_perm_set_limit(name, key, value)
}

// ─── queries (typed views) ─────────────────────────────────────────

/// Return the agent's full entitlements as a typed value.
///
/// Used by the GUI Permissions tab to render every section without
/// reparsing stdout from the CLI.
pub fn view(name: &str) -> Result<Entitlements> {
    let (_path, profile) = agent::load_profile_for_edit(name)?;
    Ok(profile.entitlements)
}
