//! `mur capability` — install a bundled capability (MCP + skills + programs +
//! entitlements) into an agent, or list/show/remove it.

use anyhow::{Result, bail};
use mur_common::AgentProfile as _AgentProfile;
use mur_common::capability::Capability;
use std::path::Path;

fn load(home: &Path, agent: &str) -> Result<(std::path::PathBuf, _AgentProfile)> {
    let path = home.join("agents").join(agent).join("profile.yaml");
    if !path.exists() {
        bail!("agent '{agent}' not found");
    }
    let profile = serde_yaml_ng::from_str(&std::fs::read_to_string(&path)?)?;
    Ok((path, profile))
}

fn union_extend(dst: &mut Vec<String>, src: &[String]) {
    for s in src {
        if !dst.iter().any(|d| d == s) {
            dst.push(s.clone());
        }
    }
}

/// Materialize `cap` into agent `agent` under `home`. Idempotent.
pub(crate) fn install_capability(home: &Path, agent: &str, cap: &Capability) -> Result<bool> {
    let (path, mut profile) = load(home, agent)?;
    // 1. MCP servers: upsert by name + allow the command to be spawned.
    //    Preserve a user's pin / security metadata (binary_sha256,
    //    description_hash, publisher, installed_at — set by `mur agent mcp
    //    pin`) across a re-install, so re-installing a capability never strips
    //    a pin the user added.
    for entry in &cap.mcp_servers {
        let prior = profile
            .mcp_servers
            .iter()
            .find(|s| s.name == entry.name)
            .cloned();
        profile.mcp_servers.retain(|s| s.name != entry.name);
        let mut new_entry = entry.clone();
        if let Some(p) = prior {
            new_entry.binary_sha256 = new_entry.binary_sha256.or(p.binary_sha256);
            new_entry.description_hash = new_entry.description_hash.or(p.description_hash);
            new_entry.publisher = new_entry.publisher.or(p.publisher);
            new_entry.installed_at = new_entry.installed_at.or(p.installed_at);
        }
        profile.mcp_servers.push(new_entry);
        if !profile
            .entitlements
            .processes
            .spawn
            .allowed
            .iter()
            .any(|a| a == &entry.command)
        {
            profile
                .entitlements
                .processes
                .spawn
                .allowed
                .push(entry.command.clone());
        }
    }
    // 2. requires_programs: merge (dedup by name).
    for dep in &cap.requires_programs {
        if !profile.requires_programs.iter().any(|d| d.name == dep.name) {
            profile.requires_programs.push(dep.clone());
        }
    }
    // 3. entitlements union.
    union_extend(
        &mut profile.entitlements.processes.spawn.allowed,
        &cap.entitlements.spawn_programs,
    );
    union_extend(
        &mut profile.entitlements.network.outbound.allow_hosts,
        &cap.entitlements.network_hosts,
    );
    union_extend(
        &mut profile.entitlements.filesystem.read,
        &cap.entitlements.filesystem_read,
    );
    // 4. requires_capabilities.
    if !profile.requires_capabilities.iter().any(|c| c == &cap.name) {
        profile.requires_capabilities.push(cap.name.clone());
    }
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    Ok(true)
}

/// Remove the MCP wiring `cap` added + drop it from `requires_capabilities`.
/// Keeps entitlements + `requires_programs` (may be shared).
pub(crate) fn remove_capability(home: &Path, agent: &str, cap: &Capability) -> Result<bool> {
    let (path, mut profile) = load(home, agent)?;
    // Remove only an MCP server that STILL matches the capability's definition
    // by SEMANTIC identity (command/args/network/…, ignoring pin metadata — see
    // `mcp_semantically_matches`). A same-named entry the user has modified is
    // left in place + warned, so `remove` never clobbers a user's edits.
    for def in &cap.mcp_servers {
        let before = profile.mcp_servers.len();
        profile
            .mcp_servers
            .retain(|s| !mcp_semantically_matches(s, def));
        if profile.mcp_servers.len() == before
            && profile.mcp_servers.iter().any(|s| s.name == def.name)
        {
            eprintln!(
                "MCP server '{}' was modified since install — leaving it in place (remove it manually if intended)",
                def.name
            );
        }
    }
    profile.requires_capabilities.retain(|c| c != &cap.name);
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    Ok(true)
}

/// Whether an installed MCP entry still matches the capability's definition by
/// its SEMANTIC identity — name/command/args/network/timeout/url/auth/programs.
/// Ignores install/security metadata (`binary_sha256`, `description_hash`,
/// `publisher`, `installed_at`) that `mur agent mcp pin` sets, so a pinned but
/// otherwise-unmodified entry is still removable, while a real edit to what the
/// server IS (command/args/network/…) is treated as user-modified and kept.
fn mcp_semantically_matches(
    entry: &mur_common::agent::McpServerEntry,
    def: &mur_common::agent::McpServerEntry,
) -> bool {
    entry.name == def.name
        && entry.command == def.command
        && entry.args == def.args
        && entry.network == def.network
        && entry.timeout_secs == def.timeout_secs
        && entry.url == def.url
        && entry.auth == def.auth
        && entry.requires_programs == def.requires_programs
}

/// `mur capability list [--agent X]`
pub fn cmd_capability_list(agent: Option<&str>) -> Result<()> {
    let installed: Vec<String> = match agent {
        Some(a) => {
            crate::cmd::agent::load_profile_for_edit(a)?
                .1
                .requires_capabilities
        }
        None => Vec::new(),
    };
    for c in crate::capabilities::builtin_capabilities() {
        let mark = if installed.iter().any(|n| n == &c.name) {
            " [installed]"
        } else {
            ""
        };
        println!("{}  {}{}", c.name, c.description, mark);
    }
    Ok(())
}

/// `mur capability show <name>`
pub fn cmd_capability_show(name: &str) -> Result<()> {
    let cap = crate::capabilities::find_builtin(name)
        .ok_or_else(|| anyhow::anyhow!("capability '{name}' not found"))?;
    print!("{}", serde_yaml_ng::to_string(&cap)?);
    Ok(())
}

/// `mur capability install <name> --agent X [--yes]`
pub fn cmd_capability_install(name: &str, agent: &str, yes: bool) -> Result<()> {
    let cap = crate::capabilities::find_builtin(name)
        .ok_or_else(|| anyhow::anyhow!("capability '{name}' not found"))?;
    if !confirm_install(&cap, yes)? {
        bail!("install cancelled");
    }
    let home = crate::cmd::agent::resolve_mur_home()?;
    install_capability(&home, agent, &cap)?;
    println!("Installed capability '{name}' onto '{agent}'. Restart the agent to apply.");
    Ok(())
}

/// `mur capability remove <name> --agent X`
pub fn cmd_capability_remove(name: &str, agent: &str) -> Result<()> {
    let cap = crate::capabilities::find_builtin(name)
        .ok_or_else(|| anyhow::anyhow!("capability '{name}' not found"))?;
    let home = crate::cmd::agent::resolve_mur_home()?;
    remove_capability(&home, agent, &cap)?;
    println!(
        "Removed capability '{name}' from '{agent}' (MCP servers matching the definition + requires_capabilities). Restart the agent to apply."
    );
    let ent = &cap.entitlements;
    if !ent.network_hosts.is_empty()
        || !ent.spawn_programs.is_empty()
        || !ent.filesystem_read.is_empty()
    {
        println!(
            "Entitlements this capability had requested were KEPT (another capability or manual config may rely on them):"
        );
        if !ent.network_hosts.is_empty() {
            println!("  network hosts: {}", ent.network_hosts.join(", "));
        }
        if !ent.spawn_programs.is_empty() {
            println!("  spawn programs: {}", ent.spawn_programs.join(", "));
        }
        if !ent.filesystem_read.is_empty() {
            println!("  filesystem read: {}", ent.filesystem_read.join(", "));
        }
        println!("  To revoke manually, use `mur agent perm` (see `mur agent perm --help`).");
    }
    Ok(())
}

fn confirm_install(cap: &Capability, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    println!("Capability '{}' will grant on this agent:", cap.name);
    for e in &cap.mcp_servers {
        println!("  MCP server: {} ({})", e.name, e.command);
    }
    for d in &cap.requires_programs {
        println!("  requires program: {}", d.name);
    }
    if !cap.entitlements.network_hosts.is_empty() {
        println!(
            "  network hosts: {}",
            cap.entitlements.network_hosts.join(", ")
        );
    }
    use std::io::{self, IsTerminal, Write};
    if !io::stdin().is_terminal() {
        bail!("not a TTY — re-run with --yes to install non-interactively");
    }
    print!("Proceed? [y/N] ");
    io::stdout().flush().ok();
    let mut ans = String::new();
    io::stdin().read_line(&mut ans)?;
    Ok(matches!(
        ans.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_PROFILE: &str = "\
schema: 1
id: 0192f5a1-28ab-7111-8000-000000000099
name: a
display_name: \"A\"
version: \"0.1.0\"
persona:
  category: research
  description: \"test profile\"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: \"sys_prompt.md\"
model: { provider: ollama, name: \"m\", params: {} }
mcp_servers: []
skills: []
transport: { stdio: true, socket: { enabled: true, bind: \"unix:///tmp/a.sock\" } }
communication: { accepts_from: [\"*\"], sends_to: [] }
capabilities: [\"a2a.message.send\",\"a2a.tasks\"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: [\"tcp\"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: [\"~/.ssh\"] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [\"rate_limit\"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: \"2026-04-22T10:00:00+08:00\"
updated_at: \"2026-04-22T10:00:00+08:00\"
";

    fn seed_agent(home: &Path, agent: &str) {
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profile.yaml"), MINIMAL_PROFILE).unwrap();
    }
    fn media() -> Capability {
        crate::capabilities::find_builtin("media").unwrap()
    }

    #[test]
    fn install_materializes_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_agent(home, "a");
        install_capability(home, "a", &media()).unwrap();
        install_capability(home, "a", &media()).unwrap();
        let p: _AgentProfile = serde_yaml_ng::from_str(
            &std::fs::read_to_string(home.join("agents/a/profile.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            p.mcp_servers.iter().filter(|s| s.name == "media").count(),
            1
        );
        assert!(p.requires_programs.iter().any(|d| d.name == "vlc"));
        assert!(p.requires_capabilities.iter().any(|c| c == "media"));
        assert!(
            p.entitlements
                .network
                .outbound
                .allow_hosts
                .iter()
                .any(|h| h == "127.0.0.1")
        );
        assert!(
            p.entitlements
                .processes
                .spawn
                .allowed
                .iter()
                .any(|c| c == "mur-mcp-server")
        );
    }

    #[test]
    fn remove_reverses_mcp_and_requires_capabilities_but_keeps_programs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_agent(home, "a");
        install_capability(home, "a", &media()).unwrap();
        remove_capability(home, "a", &media()).unwrap();
        let p: _AgentProfile = serde_yaml_ng::from_str(
            &std::fs::read_to_string(home.join("agents/a/profile.yaml")).unwrap(),
        )
        .unwrap();
        assert!(!p.mcp_servers.iter().any(|s| s.name == "media"));
        assert!(!p.requires_capabilities.iter().any(|c| c == "media"));
        assert!(p.requires_programs.iter().any(|d| d.name == "vlc"));
    }

    #[test]
    fn remove_keeps_a_user_modified_mcp_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_agent(home, "a");
        install_capability(home, "a", &media()).unwrap();

        // User edits the installed media MCP entry (changes the timeout).
        let path = home.join("agents/a/profile.yaml");
        let mut p: _AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let e = p
            .mcp_servers
            .iter_mut()
            .find(|s| s.name == "media")
            .unwrap();
        e.timeout_secs = Some(999);
        crate::cmd::agent::save_profile(&path, &mut p).unwrap();

        remove_capability(home, "a", &media()).unwrap();

        let p2: _AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // The modified entry is KEPT (not clobbered)…
        assert!(
            p2.mcp_servers
                .iter()
                .any(|s| s.name == "media" && s.timeout_secs == Some(999)),
            "a user-modified MCP entry must be kept, not removed"
        );
        // …but requires_capabilities is still dropped.
        assert!(!p2.requires_capabilities.iter().any(|c| c == "media"));
    }

    #[test]
    fn reinstall_preserves_pin_and_remove_still_removes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_agent(home, "a");
        install_capability(home, "a", &media()).unwrap();

        // User pins the media MCP binary (sets binary_sha256); semantics unchanged.
        let path = home.join("agents/a/profile.yaml");
        let mut p: _AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        p.mcp_servers
            .iter_mut()
            .find(|s| s.name == "media")
            .unwrap()
            .binary_sha256 = Some("deadbeef".into());
        crate::cmd::agent::save_profile(&path, &mut p).unwrap();

        // Re-install must NOT strip the pin.
        install_capability(home, "a", &media()).unwrap();
        let p2: _AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            p2.mcp_servers
                .iter()
                .find(|s| s.name == "media")
                .unwrap()
                .binary_sha256
                .as_deref(),
            Some("deadbeef"),
            "re-install must preserve a user's pin"
        );

        // Remove: a pinned-but-otherwise-unmodified entry is still removed
        // (the pin is ignored by the semantic match).
        remove_capability(home, "a", &media()).unwrap();
        let p3: _AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            !p3.mcp_servers.iter().any(|s| s.name == "media"),
            "a pinned-but-unmodified MCP entry must still be removable"
        );
    }
}
