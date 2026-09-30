//! TUI-safe agent management for `/mcp` and `/skill` slash commands.
//!
//! The `mur agent mcp|skill` CLI handlers print to stdout and (for installs)
//! prompt on stdin — both unusable inside a raw-mode alternate screen. These
//! variants return the text to render and never touch the terminal; the
//! silent CLI helpers (`cmd_mcp_remove`, `cmd_skill_add`, `cmd_skill_remove`)
//! are reused as-is.

use anyhow::{Result, bail};
use mur_common::agent::McpServerEntry;
use mur_common::proposal::Proposal;

use crate::cmd::agent::{load_profile_for_edit, save_profile};

/// Line appended after any profile mutation: the supervisor only reads the
/// profile at startup. The restart itself is offered as a chip ([`applied`]),
/// not spelled out as a command to retype.
pub const RESTART_HINT: &str = "profile updated — restart the agent to apply";

/// Chip label for the restart a profile mutation needs.
pub const RESTART_LABEL: &str = "apply the profile change";

/// A manage command's result: text to show, plus a proposal for the chip.
pub type Managed = (String, Option<Proposal>);

/// `text` + the restart hint line, with the restart offered as a chip.
fn applied(text: String) -> Managed {
    (
        format!("{text}\n{RESTART_HINT}"),
        Some(Proposal::restart(RESTART_LABEL)),
    )
}

/// A read-only or usage result: no restart needed.
fn plain(text: impl Into<String>) -> Managed {
    (text.into(), None)
}

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
pub fn mcp_add(agent: &str, server_id: &str, command: &str, args: &[String]) -> Result<Managed> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    if profile.mcp_servers.iter().any(|s| s.name == server_id) {
        bail!("MCP server '{server_id}' already exists on '{agent}'");
    }

    let mut notes = Vec::new();
    let (binary_sha256, resolved_path) = match crate::cmd::agent_mcp_pin::resolve_command(command) {
        Ok(p) => {
            let sha = match crate::cmd::agent_mcp_pin::compute_binary_sha256(&p) {
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
            };
            (sha, Some(p))
        }
        Err(_) => {
            notes.push(format!(
                "warning: `{command}` not found on PATH; no binary pin"
            ));
            (None, None)
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
        state_paths: Vec::new(),
        package: None,
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
    // Same gate as `mur agent mcp add`, through the same function: a slash
    // command that reports "added" for a server which cannot start produces
    // the same unbootable agent, and this path used to skip the check purely
    // because it reimplements the install rather than calling it.
    if let Some(resolved) = resolved_path.as_deref() {
        let (hash, tools) =
            crate::cmd::agent::mcp_add::probe_new_entry(agent, &profile, server_id, resolved)?;
        if let Some(e) = profile.mcp_servers.last_mut() {
            e.description_hash = Some(hash);
        }
        notes.push(format!(
            "probe ok — {tools} tool{} listed, description hash pinned",
            if tools == 1 { "" } else { "s" }
        ));
    }

    save_profile(&path, &mut profile)?;

    let mut out = format!(
        "added MCP server '{server_id}' ({command} {})",
        args.join(" ")
    );
    for n in notes {
        out.push_str(&format!("\n  {n}"));
    }
    Ok(applied(out))
}

pub fn mcp_remove(agent: &str, server_id: &str) -> Result<Managed> {
    crate::cmd::agent::mcp::cmd_mcp_remove(agent, server_id)?;
    Ok(applied(format!("removed MCP server '{server_id}'")))
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

pub fn skill_add(agent: &str, source: &str) -> Result<Managed> {
    crate::cmd::agent::skill::cmd_skill_add(agent, source)?;
    Ok(applied(format!("installed skill from '{source}'")))
}

/// Second step of the two-step `/browser --add` flow. `--add` installs the
/// skill and grants nothing: the browser skill additionally needs
/// `allow-spawn` for `playwright-mcp` and `chrome-headless-shell`, plus
/// `allow-spawn-dir` for the agent's `shim/probe` directory (`~/.mur` is
/// writable but not executable). Those are privilege escalations, so they are
/// never applied silently here — `mur browser setup` owns them and asks first.
pub const BROWSER_SETUP_HINT: &str =
    "then run `mur browser setup` to grant the browser skill what it needs to spawn";

/// `/browser --add`: install the skill, then point at the setup step that
/// performs the grants. The restart chip stays the offered action — the
/// profile just changed and the supervisor only reads it at startup.
pub fn browser_skill_add(agent: &str, source: &str) -> Result<Managed> {
    let (text, chip) = skill_add(agent, source)?;
    Ok((format!("{text}\n{BROWSER_SETUP_HINT}"), chip))
}

pub fn skill_remove(agent: &str, query: &str) -> Result<Managed> {
    crate::cmd::agent::skill::cmd_skill_remove(agent, query)?;
    Ok(applied(format!("removed skill '{query}'")))
}

/// Usage strings shown for bad arguments.
pub const MCP_USAGE: &str =
    "usage: /mcp [list] · /mcp add <name> <command> [args…] · /mcp remove <name>";
pub const SKILL_USAGE: &str = "usage: /skill [list] · /skill add <path> (validates + installs a .yaml/.md skill into skills/<name>/skill.yaml) · /skill remove <name>";

/// Dispatch a parsed `/mcp` invocation.
pub fn run_mcp(agent: &str, args: &[String]) -> Result<Managed> {
    match args.first().map(String::as_str) {
        None | Some("list") => mcp_list(agent).map(plain),
        Some("add") if args.len() >= 3 => mcp_add(agent, &args[1], &args[2], &args[3..]),
        Some("remove") | Some("rm") if args.len() == 2 => mcp_remove(agent, &args[1]),
        _ => Ok(plain(MCP_USAGE)),
    }
}

/// Dispatch a parsed `/skill` invocation.
pub fn run_skill(agent: &str, args: &[String]) -> Result<Managed> {
    match args.first().map(String::as_str) {
        None | Some("list") => skill_list(agent).map(plain),
        Some("add") | Some("install") if args.len() == 2 => skill_add(agent, &args[1]),
        Some("remove") | Some("rm") if args.len() == 2 => skill_remove(agent, &args[1]),
        _ => Ok(plain(SKILL_USAGE)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §4 latent bug: the hint used to carry a literal `<name>` and teach the
    /// old stop/start dance. It must now pass the same vet an agent's
    /// proposal does, and hand the restart over as a chip.
    #[test]
    fn restart_hint_is_vettable_and_offers_a_restart_chip() {
        assert!(!RESTART_HINT.contains('<'), "{RESTART_HINT}");
        let args = serde_json::json!({ "label": RESTART_LABEL, "kind": "restart" });
        assert!(mur_common::proposal::vet(&args).is_ok());
        let (text, chip) = applied("removed skill 'x'".into());
        assert!(text.ends_with(RESTART_HINT), "{text}");
        assert!(chip.is_some_and(|p| p.is_executable()));
    }

    #[test]
    fn browser_add_points_at_the_setup_step() {
        // The hint must survive the same vet an agent's proposal does (no
        // `<placeholder>`), and `--add` must never be the thing that grants.
        assert!(!BROWSER_SETUP_HINT.contains('<'), "{BROWSER_SETUP_HINT}");
        assert!(BROWSER_SETUP_HINT.contains("mur browser setup"));
    }

    #[test]
    fn read_only_results_offer_no_chip() {
        assert!(plain(MCP_USAGE).1.is_none());
        assert!(run_mcp("a", &["bogus".into()]).unwrap().1.is_none());
    }
}
