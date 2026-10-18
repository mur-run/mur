//! #1984: wire the pinned ast-grep to the agent after `mur code-nav setup`
//! installs it. Installing alone left the agent with no `ast_grep_search`
//! (no `mur-mcp-server` entry) and no way to exec the binary.
//!
//! What "wired" means, each piece proven necessary by the #1984 spike:
//! - an MCP entry for MUR's own server, launched from the bundled copy the
//!   runtime actually execs (`resolve_mcp_launch_in` redirects any
//!   `mur-mcp-server` command there), so the spawn grant names that file;
//! - a spawn grant on the pinned ast-grep binary by absolute path: the MCP
//!   child inherits the agent's sandbox, and a bare `ast-grep` resolves
//!   through the system search dirs, never MUR's tools dir;
//! - read+write on the tool's run directory, where it writes an empty
//!   `sgconfig.yml` on every call.
//!
//! An agent that already has a `mur-mcp-server` entry (e.g. the media
//! capability) keeps it: that server already serves `ast_grep_search`, so
//! only the grants are added. The entry's pin is left to the runtime's
//! first-party re-pin at the next start.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use mur_common::AgentProfile;
use mur_common::agent::McpServerEntry;

use super::consent::{Manifest, grant_key};
use super::plan::Permission;

/// Name of the MCP entry setup adds when the agent has no MUR server yet.
pub const ENTRY_NAME: &str = "mur-tools";

/// Where `ast_grep_search` will come from on this agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// No MUR server on the agent: setup adds [`ENTRY_NAME`].
    Attach,
    /// This existing entry already runs MUR's server; only grants are added.
    Existing { name: String, enabled: bool },
}

/// Does `command` launch MUR's own MCP server?
pub fn is_mur_mcp_server(command: &str) -> bool {
    Path::new(command).file_name() == Some(mur_common::exec::MCP_SERVER_BIN.as_ref())
}

/// Decide the slot from the profile as it stands. Refuses a user-owned
/// server that already holds [`ENTRY_NAME`]: overwriting it would silently
/// drop their entry, and adding a second one is refused at profile save.
pub fn slot(profile: &AgentProfile) -> Result<Slot> {
    if let Some(e) = profile
        .mcp_servers
        .iter()
        .find(|e| is_mur_mcp_server(&e.command))
    {
        return Ok(Slot::Existing {
            name: e.name.clone(),
            enabled: profile.mcp_enabled(&e.name),
        });
    }
    if let Some(e) = profile.mcp_servers.iter().find(|e| e.name == ENTRY_NAME) {
        bail!(
            "MCP server `{ENTRY_NAME}` on '{}' runs `{}`, not MUR's server, so setup will \
             not touch it. Rename or remove it (`mur agent mcp remove {} {ENTRY_NAME}`), \
             or pass --no-ast-grep",
            profile.name,
            e.command,
            profile.name
        );
    }
    Ok(Slot::Attach)
}

/// [`slot`] for `agent`, read from its profile. Run before anything is
/// installed so a refusal leaves the home untouched.
pub fn preflight(agent: &str) -> Result<Slot> {
    let (_, profile) = crate::cmd::agent::load_profile_for_edit(agent)?;
    slot(&profile)
}

/// What the consent table lists for wiring. Pure: paths only.
pub fn permissions(mur_home: &Path, slot: &Slot) -> Vec<Permission> {
    let server = mur_common::exec::bundled_mcp_server_path_in(mur_home);
    let mut out = vec![
        Permission::Spawn(
            mur_common::config::ast_grep_binary_path(mur_home)
                .display()
                .to_string(),
        ),
        Permission::Spawn(server.display().to_string()),
        Permission::ReadWrite(mur_common::config::ast_grep_runtime_dir(mur_home)),
    ];
    if *slot == Slot::Attach {
        out.push(Permission::Mcp {
            name: ENTRY_NAME.to_owned(),
            command: server,
        });
    }
    out
}

/// Apply [`permissions`] to `agent`'s profile in one save, record each in
/// the manifest, and say what was wired. Re-running adds nothing twice.
pub fn apply(mur_home: &Path, agent: &str, m: &mut Manifest, out: &mut dyn Write) -> Result<()> {
    let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(agent)?;
    let slot = slot(&profile)?;
    let perms = permissions(mur_home, &slot);
    for p in &perms {
        grant(agent, &mut profile, p)?;
    }
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    for p in &perms {
        m.granted.insert(grant_key(p));
    }

    match &slot {
        Slot::Attach => writeln!(
            out,
            "  ✓ ast_grep_search: added MCP server `{ENTRY_NAME}` ({})",
            mur_common::exec::bundled_mcp_server_path_in(mur_home).display()
        )?,
        Slot::Existing { name, enabled } => {
            writeln!(
                out,
                "  ✓ ast_grep_search: served by existing MCP server `{name}`"
            )?;
            if !enabled {
                writeln!(
                    out,
                    "  ! `{name}` is disabled, so the agent will not see the tool: \
                     mur agent mcp enable {agent} {name}"
                )?;
            }
        }
    }
    writeln!(
        out,
        "  ✓ ast-grep exec + run-dir grants; takes effect on the agent's next start \
         (mur agent restart {agent} if it is running)"
    )?;
    Ok(())
}

fn grant(agent: &str, profile: &mut AgentProfile, p: &Permission) -> Result<()> {
    match p {
        Permission::Spawn(b) => push_unique(&mut profile.entitlements.processes.spawn.allowed, b),
        Permission::ReadWrite(d) => {
            let raw = path_string(d)?;
            crate::cmd::agent::reject_ungrantable_path(agent, &raw, true)?;
            // The seal drops a grant whose path does not exist yet.
            std::fs::create_dir_all(d)?;
            push_unique(&mut profile.entitlements.filesystem.read, &raw);
            push_unique(&mut profile.entitlements.filesystem.write, &raw);
        }
        Permission::Mcp { name, command } => {
            if !profile.mcp_servers.iter().any(|e| &e.name == name) {
                profile.mcp_servers.push(McpServerEntry {
                    name: name.clone(),
                    command: path_string(command)?,
                    installed_at: Some(chrono::Utc::now()),
                    ..Default::default()
                });
            }
        }
        Permission::Read(_) | Permission::SpawnDir(_) => {
            bail!("internal: ast-grep wiring does not grant {}", grant_key(p))
        }
    }
    Ok(())
}

fn push_unique(list: &mut Vec<String>, v: &str) {
    if !list.iter().any(|x| x == v) {
        list.push(v.to_owned());
    }
}

fn path_string(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("path {} is not UTF-8", p.display()))
}

/// The paths a wired agent must hold, for doctor to compare against.
pub fn required_paths(mur_home: &Path) -> (PathBuf, PathBuf) {
    (
        mur_common::config::ast_grep_binary_path(mur_home),
        mur_common::config::ast_grep_runtime_dir(mur_home),
    )
}

#[cfg(test)]
#[path = "ast_grep_wire_tests.rs"]
mod tests;
