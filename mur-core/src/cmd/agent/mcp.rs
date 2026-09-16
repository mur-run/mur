//! `mur agent mcp` — list / add / remove / rename MCP servers attached to
//! an agent profile.
//!
//! The install path lives in `mcp_add.rs` and the egress policy in
//! `mcp_network.rs`; what stays here is listing plus the rename / remove /
//! enable bookkeeping.

use anyhow::{Result, bail};
use mur_common::agent::{McpNetMode, McpServerEntry};

use super::{load_profile_for_edit, save_profile};

/// Render a single MCP server entry as a formatted line, with warning for BroadAudited mode.
fn render_server_line(s: &McpServerEntry) -> String {
    let base_line = format!("{}\t{} {}", s.name, s.command, s.args.join(" "))
        .trim_end()
        .to_string();

    if let Some(net) = &s.network
        && net.mode == McpNetMode::BroadAudited
    {
        let authorized_by = net
            .authorization
            .as_ref()
            .map(|a| a.authorized_by.as_str())
            .unwrap_or("unknown");
        let warning = format!(
            "\n  ⚠ BROAD EGRESS (audited) — allows any host except deny_hosts; authorized by {}",
            authorized_by
        );
        return base_line + &warning;
    }

    base_line
}

pub fn cmd_mcp_list(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.is_empty() {
        println!("(no MCP servers configured)");
        return Ok(());
    }
    for s in &profile.mcp_servers {
        println!("{}", render_server_line(s));
    }
    Ok(())
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

/// Enable/disable an MCP server for an agent by editing the per-agent
/// denylist. Non-destructive: the entry (and its pin) stays in the profile.
pub fn cmd_mcp_set_enabled(name: &str, server_id: &str, enabled: bool) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile.mcp_servers.iter().any(|s| s.name == server_id) {
        bail!("MCP server '{server_id}' not found on '{name}'");
    }
    profile.set_mcp_enabled(server_id, enabled);
    save_profile(&path, &mut profile)?;
    println!(
        "{} MCP server '{server_id}' for '{name}' (restart the agent to apply)",
        if enabled { "Enabled" } else { "Disabled" }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Test-only: the render path builds these by hand, but `mcp.rs` itself no
    // longer names them now that the policy lives in `mcp_network.rs`.
    use mur_common::agent::{EgressAuthorization, McpNetMode, McpServerNetwork};

    #[test]
    fn render_server_line_broad_audited_with_authorization() {
        let server = McpServerEntry {
            name: "browser".to_string(),
            command: "firefox".to_string(),
            args: vec!["--mcp".to_string()],
            binary_sha256: None,
            description_hash: None,
            publisher: None,
            installed_at: None,
            timeout_secs: None,
            network: Some(McpServerNetwork {
                mode: McpNetMode::BroadAudited,
                allow_hosts: vec![],
                deny_hosts: vec!["badhost.com".to_string()],
                authorization: Some(EgressAuthorization {
                    authorized_by: "alice".to_string(),
                    authorized_at_ms: 1_700_000_000_000,
                }),
            }),
            url: None,
            auth: None,
            requires_programs: Vec::new(),
            state_paths: vec![],
            package: None,
        };
        let output = render_server_line(&server);
        assert!(output.contains("browser\tfirefox --mcp"));
        assert!(output.contains("BROAD EGRESS (audited)"));
        assert!(output.contains("authorized by alice"));
    }

    #[test]
    fn render_server_line_broad_audited_without_authorization_fallback() {
        let server = McpServerEntry {
            name: "browser".to_string(),
            command: "firefox".to_string(),
            args: vec![],
            binary_sha256: None,
            description_hash: None,
            publisher: None,
            installed_at: None,
            timeout_secs: None,
            network: Some(McpServerNetwork {
                mode: McpNetMode::BroadAudited,
                allow_hosts: vec![],
                deny_hosts: vec![],
                authorization: None,
            }),
            url: None,
            auth: None,
            requires_programs: Vec::new(),
            state_paths: vec![],
            package: None,
        };
        let output = render_server_line(&server);
        assert!(output.contains("browser\tfirefox"));
        assert!(output.contains("BROAD EGRESS (audited)"));
        assert!(output.contains("authorized by unknown"));
    }

    #[test]
    fn render_server_line_restricted_no_warning() {
        let server = McpServerEntry {
            name: "api".to_string(),
            command: "mcp-api".to_string(),
            args: vec![],
            binary_sha256: None,
            description_hash: None,
            publisher: None,
            installed_at: None,
            timeout_secs: None,
            network: Some(McpServerNetwork {
                mode: McpNetMode::Restricted,
                allow_hosts: vec!["example.com".to_string()],
                deny_hosts: vec![],
                authorization: None,
            }),
            url: None,
            auth: None,
            requires_programs: Vec::new(),
            state_paths: vec![],
            package: None,
        };
        let output = render_server_line(&server);
        assert!(output.contains("api\tmcp-api"));
        assert!(!output.contains("BROAD EGRESS"));
        assert!(!output.contains("authorized by"));
    }

    #[test]
    fn render_server_line_no_network_no_warning() {
        let server = McpServerEntry {
            name: "local".to_string(),
            command: "local-mcp".to_string(),
            args: vec![],
            binary_sha256: None,
            description_hash: None,
            publisher: None,
            installed_at: None,
            timeout_secs: None,
            network: None,
            url: None,
            auth: None,
            requires_programs: Vec::new(),
            state_paths: vec![],
            package: None,
        };
        let output = render_server_line(&server);
        assert!(output.contains("local\tlocal-mcp"));
        assert!(!output.contains("BROAD EGRESS"));
        assert!(!output.contains("authorized by"));
    }
}
