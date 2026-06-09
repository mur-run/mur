//! `mur agent mcp` — list / add / remove / rename MCP servers attached to
//! an agent profile.

use anyhow::{Context, Result, bail};
use mur_common::agent::McpServerEntry;

use super::{load_profile_for_edit, save_profile};

/// Optional install-time pinning fields for `cmd_mcp_add` (B0 rule 6 / M9.2).
///
/// `force = true` skips the y/N confirm prompt — for scripted /
/// non-interactive installers. Publisher fields are passed through
/// from `--publisher-name` / `--publisher-homepage` /
/// `--publisher-registry-id` CLI flags; they're display-only and
/// don't affect the binary hash.
#[derive(Debug, Clone, Default)]
pub struct McpAddPin {
    pub force: bool,
    pub publisher_name: Option<String>,
    pub publisher_homepage: Option<String>,
    pub publisher_registry_id: Option<String>,
}

pub fn cmd_mcp_list(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.is_empty() {
        println!("(no MCP servers configured)");
        return Ok(());
    }
    for s in &profile.mcp_servers {
        println!("{}\t{} {}", s.name, s.command, s.args.join(" "));
    }
    Ok(())
}

pub fn cmd_mcp_add(
    name: &str,
    server_id: &str,
    command: &str,
    args: &[String],
    pin: McpAddPin,
) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.iter().any(|s| s.name == server_id) {
        bail!("MCP server '{server_id}' already exists on '{name}'");
    }

    // ── B0 rule 6 / M9.2: install-time hash + publisher prompt. ──
    // Best-effort: if the binary can't be located on PATH yet, we
    // proceed without a hash and the entry behaves as a pre-M9
    // entry (warn-but-don't-block on startup). This keeps the
    // existing `mur agent mcp add foo --command not-yet-installed`
    // workflow alive for users who add the entry before installing
    // the binary.
    let (binary_sha256, resolved_path) = match crate::cmd::agent_mcp_pin::resolve_command(command) {
        Ok(p) => match crate::cmd::agent_mcp_pin::compute_binary_sha256(&p) {
            Ok(h) => (Some(h), Some(p)),
            Err(e) => {
                eprintln!(
                    "warning: could not hash {} ({e}); entry will be installed without binary pin",
                    p.display(),
                );
                (None, Some(p))
            }
        },
        Err(_) => {
            eprintln!(
                "warning: could not resolve `{command}` on PATH; entry will be installed without binary pin",
            );
            (None, None)
        }
    };

    let publisher = pin
        .publisher_name
        .as_ref()
        .map(|n| mur_common::agent::McpPublisherInfo {
            name: n.clone(),
            homepage: pin.publisher_homepage.clone(),
            registry_id: pin.publisher_registry_id.clone(),
        });

    if !pin.force {
        // Render summary + prompt.
        println!("About to install MCP server \"{server_id}\":");
        if let Some(p) = &resolved_path {
            println!("  command:        {}", p.display());
        } else {
            println!("  command:        {command} (not yet on PATH)");
        }
        if !args.is_empty() {
            println!("  args:           {}", args.join(" "));
        }
        if let Some(p) = &publisher {
            println!("  publisher:      {}", p.name);
            if let Some(h) = &p.homepage {
                println!("                  {h}");
            }
            if let Some(r) = &p.registry_id {
                println!("                  {r}");
            }
        }
        if let Some(h) = &binary_sha256 {
            // Show a short prefix so the user can spot-check against
            // the publisher's release notes.
            println!("  binary sha256:  {}…  (full: {h})", &h[..16]);
        }
        println!(
            "  description hash: <deferred to live MCP probe — will be set on first run via M9.3>",
        );
        print!("\nApprove? [y/N] ");
        use std::io::{self, Write};
        io::stdout().flush().ok();
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .with_context(|| "read confirmation from stdin")?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            bail!("install cancelled");
        }
    }

    profile.mcp_servers.push(McpServerEntry {
        name: server_id.to_string(),
        command: command.to_string(),
        args: args.to_vec(),
        binary_sha256,
        // description_hash is populated by the runtime supervisor on
        // first successful spawn (M9.3). Leaving it `None` here keeps
        // the entry in "warn but don't block" mode until then.
        description_hash: None,
        publisher,
        installed_at: Some(chrono::Utc::now()),
        timeout_secs: None,
    });
    // Sync spawn allowlist so the supervisor is permitted to launch this MCP.
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
    save_profile(&path, &mut profile)
}

pub fn cmd_mcp_remove(name: &str, server_id: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let before = profile.mcp_servers.len();
    let removed_command = profile
        .mcp_servers
        .iter()
        .find(|s| s.name == server_id)
        .map(|s| s.command.clone());
    profile.mcp_servers.retain(|s| s.name != server_id);
    if profile.mcp_servers.len() == before {
        bail!("MCP server '{server_id}' not found on '{name}'");
    }
    // Drop the command from the spawn allowlist only if no other mcp entry
    // still needs it.
    if let Some(cmd) = removed_command
        && !profile.mcp_servers.iter().any(|s| s.command == cmd)
    {
        profile
            .entitlements
            .processes
            .spawn
            .allowed
            .retain(|a| a != &cmd);
    }
    save_profile(&path, &mut profile)
}

pub fn cmd_mcp_rename(name: &str, old: &str, new: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.iter().any(|s| s.name == new) {
        bail!("MCP server '{new}' already exists on '{name}'");
    }
    let hit = profile.mcp_servers.iter_mut().find(|s| s.name == old);
    match hit {
        Some(s) => s.name = new.to_string(),
        None => bail!("MCP server '{old}' not found on '{name}'"),
    }
    save_profile(&path, &mut profile)
}
