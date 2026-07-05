//! Skills / MCP install & remove — Hub side. Thin wrappers around the
//! `mur agent skill ...` / `mur agent mcp ...` CLI code paths so the GUI
//! and CLI share one implementation (validation, binary pinning,
//! entitlement allow-listing). Each mutation returns the refreshed
//! `AgentDetail` so the panel re-renders without a second round-trip.

use crate::detail::{AgentDetail, get_agent_detail};
use mur_core::cmd::agent::mcp::{McpAddPin, cmd_mcp_add, cmd_mcp_remove, cmd_mcp_set_enabled};
use mur_core::cmd::agent::skill::{cmd_skill_add, cmd_skill_remove, cmd_skill_set_enabled};
use mur_core::cmd::agent::skill_remote::{SkillPreview, install_any_url, preview_any_url};
use serde::Serialize;

/// Result of a skill install: the refreshed agent detail plus the id under
/// which the skill was registered, so the UI can report the real outcome
/// (the installed skill's name) instead of a blanket "installed" message.
#[derive(Debug, Clone, Serialize)]
pub struct SkillInstallResult {
    pub detail: AgentDetail,
    /// The canonical id the skill was registered as, e.g. `skills/foo.yaml`.
    pub installed_id: String,
}

#[tauri::command]
pub fn agent_skill_install(
    name: String,
    source_path: String,
) -> Result<SkillInstallResult, String> {
    cmd_skill_add(&name, &source_path).map_err(|e| format!("{e:#}"))?;
    // Mirror `cmd_skill_add`'s id derivation: the source basename, registered
    // under `skills/<basename>`. Computed only after a successful add.
    let installed_id = std::path::Path::new(&source_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|b| format!("skills/{b}"))
        .unwrap_or_else(|| source_path.clone());
    let detail = get_agent_detail(name)?;
    Ok(SkillInstallResult {
        detail,
        installed_id,
    })
}

#[tauri::command]
pub fn agent_skill_uninstall(name: String, skill_id: String) -> Result<AgentDetail, String> {
    cmd_skill_remove(&name, &skill_id).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_mcp_add(
    name: String,
    server_id: String,
    command: String,
    args: Vec<String>,
) -> Result<AgentDetail, String> {
    let server_id = server_id.trim();
    let command = command.trim();
    if server_id.is_empty() {
        return Err("server id must not be empty".into());
    }
    if command.is_empty() {
        return Err("command must not be empty".into());
    }
    // force=true: the GUI itself is the confirmation step (explicit form
    // submit), so skip the CLI's interactive y/N prompt.
    cmd_mcp_add(
        &name,
        server_id,
        command,
        &args,
        McpAddPin {
            force: true,
            ..Default::default()
        },
    )
    .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_mcp_remove(name: String, server_id: String) -> Result<AgentDetail, String> {
    cmd_mcp_remove(&name, &server_id).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

/// Non-destructive enable/disable of an installed skill (Phase-1 denylist).
#[tauri::command]
pub fn agent_skill_toggle(
    name: String,
    skill_id: String,
    enabled: bool,
) -> Result<AgentDetail, String> {
    cmd_skill_set_enabled(&name, &skill_id, enabled).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

/// Non-destructive enable/disable of a configured MCP server (Phase-1 denylist).
#[tauri::command]
pub fn agent_mcp_toggle(
    name: String,
    server_id: String,
    enabled: bool,
) -> Result<AgentDetail, String> {
    cmd_mcp_set_enabled(&name, &server_id, enabled).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

// ─── Add-on (Phase-2) commands ────────────────────────────────────────────────

/// Import a Claude plugin directory as a disabled add-on (fail-closed).
/// `force` skips the interactive confirm prompt; `None` defaults to `false`.
#[tauri::command]
pub fn agent_addon_import(
    name: String,
    plugin_dir: String,
    force: Option<bool>,
) -> Result<AgentDetail, String> {
    mur_core::cmd::agent::addon::cmd_addon_import(&name, &plugin_dir, None, force.unwrap_or(false))
        .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

/// Enable or disable an installed add-on group (Phase-2 group denylist).
/// Only `enabled=true` activates an add-on — import always lands disabled.
#[tauri::command]
pub fn agent_addon_toggle(
    name: String,
    addon_id: String,
    enabled: bool,
) -> Result<AgentDetail, String> {
    mur_core::cmd::agent::addon::cmd_addon_set_enabled(&name, &addon_id, enabled)
        .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

/// Permanently remove an add-on: deletes skill dirs, MCP entries, and the
/// `AddonRef`. Non-addon skills and agents are untouched.
#[tauri::command]
pub fn agent_addon_remove(name: String, addon_id: String) -> Result<AgentDetail, String> {
    mur_core::cmd::agent::addon::cmd_addon_remove(&name, &addon_id)
        .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

// ─── Remote MCP (Tasks 5-7) ───────────────────────────────────────────────

/// Probe a remote (Streamable-HTTP) MCP server before saving it. Reports
/// transport variant, whether auth is required, and the tool list for the
/// consent screen. Read-only; writes nothing to disk or keychain.
#[tauri::command]
pub async fn agent_mcp_test_connection(
    url: String,
    bearer: Option<String>,
) -> Result<mur_core::cmd::agent::mcp_remote::ProbeOutcome, String> {
    mur_core::cmd::agent::mcp_remote::probe_remote(&url, bearer.as_deref())
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Add a remote (Streamable-HTTP) MCP server by URL with an optional bearer
/// token. Stores the token in the OS keychain (account
/// `<agent>/mcp/<server_id>`), pins the tool-schema hash, and defaults the
/// egress allowlist to the server's own host.
#[tauri::command]
pub async fn agent_mcp_add_remote(
    name: String,
    server_id: String,
    url: String,
    bearer: Option<String>,
) -> Result<AgentDetail, String> {
    use mur_core::cmd::agent::mcp::cmd_mcp_add_remote;
    use mur_core::cmd::agent::mcp_remote::{
        compute_probe_description_hash, probe_remote, store_remote_mcp_bearer, validate_remote_url,
    };

    let url = validate_remote_url(&url).map_err(|e| format!("{e:#}"))?;
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));

    // Probe to capture tool-schema hash for pinning (best-effort; an empty
    // tool list still allows adding, just without a hash pin).
    let outcome = probe_remote(&url, bearer.as_deref())
        .await
        .map_err(|e| format!("{e:#}"))?;
    let desc_hash = compute_probe_description_hash(&outcome.tools);

    // Store bearer token in keychain; build the matching SecretRef.
    // An empty/blank bearer is treated as no-auth — no token is stored.
    let secret_ref = if let Some(tok) = bearer.filter(|s| !s.trim().is_empty()) {
        Some(
            store_remote_mcp_bearer(&name, &server_id, &tok)
                .await
                .map_err(|e| format!("{e:#}"))?,
        )
    } else {
        None
    };

    cmd_mcp_add_remote(
        &name,
        &server_id,
        &url,
        secret_ref,
        desc_hash,
        host.as_deref(),
    )
    .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

/// Add a remote MCP server entry (URL only) then run the OAuth 2.1 + PKCE
/// login flow against it (opens the system browser). The token is stored by
/// the login flow; the entry's auth field becomes OAuth on success.
#[tauri::command]
pub async fn agent_mcp_oauth_login(
    name: String,
    server_id: String,
    url: String,
) -> Result<AgentDetail, String> {
    use mur_core::cmd::agent::mcp::cmd_mcp_add_remote;
    use mur_core::cmd::agent::mcp_login::cmd_mcp_login;
    use mur_core::cmd::agent::mcp_remote::validate_remote_url;

    let url = validate_remote_url(&url).map_err(|e| format!("{e:#}"))?;
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));

    // Create the entry first — cmd_mcp_login looks it up by server_id, then
    // reads the url from the saved entry to start the OAuth flow.
    cmd_mcp_add_remote(&name, &server_id, &url, None, None, host.as_deref())
        .map_err(|e| format!("{e:#}"))?;

    // Run OAuth 2.1 + PKCE (opens system browser; token stored by login flow).
    // TODO(feather): OAuth consent + tool-schema hash pin happen post-login (deferred).
    if let Err(e) = cmd_mcp_login(&name, &server_id).await {
        // Roll back the orphan entry so the user can retry without "already exists".
        let _ = mur_core::cmd::agent::mcp::cmd_mcp_remove(&name, &server_id);
        return Err(format!("{e:#}"));
    }

    get_agent_detail(name)
}

// ── Discovery (#2) + reveal-in-Finder (#3) ──────────────────────────────────

use mur_core::cmd::agent::mcp_discover::{DiscoveredServer, default_clients, discover};
use std::path::{Path, PathBuf};

/// Scan other installed tools (Claude Desktop/Code, Cursor, VS Code, Windsurf,
/// Antigravity, Gemini CLI, Codex) for MCP servers the user can import. The
/// frontend imports a chosen one via the existing `agent_mcp_add` command.
#[tauri::command]
pub fn mcp_discover() -> Result<Vec<DiscoveredServer>, String> {
    let home = dirs::home_dir().ok_or_else(|| "cannot resolve home directory".to_string())?;
    Ok(discover(&default_clients(), &home))
}

/// Resolve the path to reveal: an absolute path is used as-is; a relative path
/// is taken under the agent home (`<mur_home>/agents/<agent>/<rel>`) when an
/// agent is given, else under `mur_home` itself.
fn resolve_reveal_target(path: &str, agent: Option<&str>, mur_home: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(a) = agent {
        mur_home.join("agents").join(a).join(path)
    } else {
        mur_home.join(path)
    }
}

/// Reveal a skill / MCP / discovered-config item on disk. A directory is opened;
/// a file is revealed (selected) in the OS file manager.
#[tauri::command]
pub fn reveal_in_finder(path: String, agent: Option<String>) -> Result<(), String> {
    let target = resolve_reveal_target(&path, agent.as_deref(), &crate::mur_home_path());
    if !target.exists() {
        return Err(format!("path not found: {}", target.display()));
    }
    reveal_os(&target).map_err(|e| e.to_string())
}

/// Result of a URL install: the refreshed agent detail plus all skill ids
/// that were installed (one for a single skill, many for a bundle).
#[derive(Debug, Clone, Serialize)]
pub struct BundleInstallResult {
    pub detail: AgentDetail,
    /// Canonical ids of every skill installed, e.g. `["skills/foo.yaml", "skills/bar.yaml"]`.
    pub installed_ids: Vec<String>,
}

/// Fetch, parse, and security-scan a remote skill URL for user review.
/// Works for both single-skill URLs and archive bundles — returns one
/// `SkillPreview` per discovered skill. Installs nothing.
#[tauri::command]
pub async fn agent_skill_preview_url(url: String) -> Result<Vec<SkillPreview>, String> {
    preview_any_url(&url).await.map_err(|e| format!("{e:#}"))
}

/// Fetch and install a skill (or bundle) from a remote URL onto `name` agent.
/// `accept_findings` gates skills with blocking scan findings; clean skills
/// install regardless.
#[tauri::command]
pub async fn agent_skill_install_url(
    name: String,
    url: String,
    accept_findings: bool,
) -> Result<BundleInstallResult, String> {
    let installed_ids = install_any_url(&name, &url, accept_findings)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let detail = get_agent_detail(name)?;
    Ok(BundleInstallResult {
        detail,
        installed_ids,
    })
}

// ─── Skill registry (quill C — per-agent registry browse/install) ─────────────

use mur_core::cmd::agent::skill_registry_add::{
    ConsentInfo, RegistrySkillEntryView, cmd_skill_registry_add, registry_search_for_agent,
    resolve_consent,
};

/// Search the shared skill registry by keyword. Returns registry entry views
/// for the Hub "Browse registry" panel. Installs nothing.
#[tauri::command]
pub async fn agent_skill_registry_search(
    query: String,
) -> Result<Vec<RegistrySkillEntryView>, String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| format!("{e:#}"))?;
    registry_search_for_agent(&home, &query).map_err(|e| format!("{e:#}"))
}

/// Build a consent bundle for a registry skill (hash-check + sig-verify +
/// content-scan). Installs nothing — used to populate the Hub consent modal.
#[tauri::command]
pub async fn agent_skill_registry_preview(
    skill: String,
    version: Option<String>,
) -> Result<ConsentInfo, String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| format!("{e:#}"))?;
    resolve_consent(&home, &skill, version.as_deref()).map_err(|e| format!("{e:#}"))
}

/// Install a registry skill onto `name` agent at Sandboxed trust.
/// `accept` gates `needs_ack` skills; blocking skills always abort.
/// Returns the refreshed `AgentDetail` on success.
#[tauri::command]
pub async fn agent_skill_registry_install(
    name: String,
    skill: String,
    version: Option<String>,
    accept: bool,
) -> Result<AgentDetail, String> {
    cmd_skill_registry_add(&name, &skill, version.as_deref(), accept)
        .await
        .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

/// Trust a publisher key via TOFU from the Hub consent screen.
///
/// Fail-closed: a revoked key is never trustable regardless of the caller.
/// Idempotent: if the key is already in the trusted list nothing is written.
#[tauri::command]
pub async fn agent_skill_trust_publisher(
    key_fp: String,
    name: Option<String>,
) -> Result<(), String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| format!("{e:#}"))?;
    let mut kr = mur_common::skill::publisher_trust::PublisherKeyring::load_or_seed(&home)
        .map_err(|e| format!("{e:#}"))?;
    if kr.revoked.contains(&key_fp) {
        return Err(format!("key {key_fp} revoked"));
    }
    if !kr.publishers.iter().any(|p| p.key_fp == key_fp) {
        kr.publishers
            .push(mur_common::skill::publisher_trust::TrustedPublisher {
                name: name.unwrap_or_else(|| "user-trusted".to_string()),
                key_fp,
                comment: "TOFU (Hub)".to_string(),
            });
        kr.save(&home).map_err(|e| format!("{e:#}"))?;
    }
    Ok(())
}

fn reveal_os(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        if path.is_dir() {
            cmd.arg(path);
        } else {
            cmd.arg("-R").arg(path);
        }
        cmd.spawn().map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        let mut cmd = std::process::Command::new("explorer");
        if path.is_dir() {
            cmd.arg(path);
        } else {
            cmd.arg("/select,").arg(path);
        }
        cmd.spawn().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map(|_| ())
    }
}

// ─── Installed-MCP-servers list (Hub Library page) ─────────────────────────

/// One installed MCP server as shown in the MCP Servers library list,
/// aggregated across every agent that has it configured.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstalledMcpView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub transport: String,
    pub agents: Vec<String>,
}

/// Pure aggregation: given each agent's configured MCP servers, fold servers
/// that share the same identity (name) into one `InstalledMcpView` whose
/// `agents` list names every agent using it. Kept free of I/O so it's
/// unit-testable without touching `~/.mur`.
fn aggregate_mcp_servers(
    by_agent: &std::collections::BTreeMap<String, Vec<mur_common::agent::McpServerEntry>>,
) -> Vec<InstalledMcpView> {
    let mut views: std::collections::BTreeMap<String, InstalledMcpView> =
        std::collections::BTreeMap::new();

    for (agent_name, servers) in by_agent {
        for server in servers {
            let entry = views
                .entry(server.name.clone())
                .or_insert_with(|| InstalledMcpView {
                    id: server.name.clone(),
                    name: server.name.clone(),
                    description: match &server.url {
                        Some(url) => url.clone(),
                        None => {
                            let mut parts = vec![server.command.clone()];
                            parts.extend(server.args.clone());
                            parts.join(" ")
                        }
                    },
                    transport: if server.url.is_some() {
                        "remote".to_string()
                    } else {
                        "stdio".to_string()
                    },
                    agents: Vec::new(),
                });
            if !entry.agents.contains(agent_name) {
                entry.agents.push(agent_name.clone());
            }
        }
    }

    let mut result: Vec<InstalledMcpView> = views.into_values().collect();
    for v in &mut result {
        v.agents.sort();
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

/// List every MCP server configured on any agent, folding duplicates
/// (same server on multiple agents) into one entry with an `agents` list.
/// Fail-open: an unreadable agent profile is skipped (+ `tracing::warn`);
/// a missing/unreadable agents dir yields an empty list.
#[tauri::command]
pub fn mcp_installed() -> Result<Vec<InstalledMcpView>, String> {
    let mur_home = crate::mur_home_path();
    let agents_dir = mur_home.join("agents");

    if !agents_dir.exists() {
        return Ok(vec![]);
    }

    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %agents_dir.display(), error = %e, "mcp_installed: cannot read agents dir");
            return Ok(vec![]);
        }
    };

    let mut by_agent: std::collections::BTreeMap<String, Vec<mur_common::agent::McpServerEntry>> =
        std::collections::BTreeMap::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(agent_name) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let profile_path = dir.join("profile.yaml");
        match std::fs::read(&profile_path) {
            Ok(bytes) => match serde_yaml_ng::from_slice::<mur_common::AgentProfile>(&bytes) {
                Ok(profile) => {
                    by_agent.insert(agent_name.to_string(), profile.mcp_servers.clone());
                }
                Err(e) => {
                    tracing::warn!(agent = %agent_name, error = %e, "mcp_installed: skipping unparsable profile");
                }
            },
            Err(e) => {
                tracing::warn!(agent = %agent_name, error = %e, "mcp_installed: skipping unreadable profile");
            }
        }
    }

    Ok(aggregate_mcp_servers(&by_agent))
}

// ─── Add-ons (Phase-2 Plugins library) ────────────────────────────────────

/// Per-agent enabled state of an aggregated add-on, for the Plugins library
/// "used by" list.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AddonAgentState {
    pub agent: String,
    pub enabled: bool,
}

/// One add-on folded across every agent that has it installed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstalledAddonAgg {
    pub id: String,
    pub source: String,
    pub skill_count: usize,
    pub mcp_count: usize,
    pub command_count: usize,
    pub agents: Vec<AddonAgentState>,
}

/// Pure aggregation: given each agent's configured add-ons, fold add-ons
/// that share the same `id` into one `InstalledAddonAgg` whose `agents` list
/// carries the per-agent enabled state. Skill/MCP/command counts are taken
/// from the first agent seen with that add-on (an add-on's contents are the
/// same everywhere it's installed). Kept free of I/O so it's unit-testable
/// without touching `~/.mur`.
fn aggregate_addons(
    by_agent: &std::collections::BTreeMap<String, Vec<mur_common::agent::AddonRef>>,
) -> Vec<InstalledAddonAgg> {
    let mut views: std::collections::BTreeMap<String, InstalledAddonAgg> =
        std::collections::BTreeMap::new();

    for (agent_name, addons) in by_agent {
        for addon in addons {
            let entry = views
                .entry(addon.id.clone())
                .or_insert_with(|| InstalledAddonAgg {
                    id: addon.id.clone(),
                    source: addon.source.clone(),
                    skill_count: addon.skills.len(),
                    mcp_count: addon.mcp.len(),
                    command_count: addon.commands.len(),
                    agents: Vec::new(),
                });
            entry.agents.push(AddonAgentState {
                agent: agent_name.clone(),
                enabled: addon.enabled,
            });
        }
    }

    let mut result: Vec<InstalledAddonAgg> = views.into_values().collect();
    for v in &mut result {
        v.agents.sort_by(|a, b| a.agent.cmp(&b.agent));
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    result
}

/// List every add-on (imported Claude plugin) configured on any agent,
/// folding duplicates (same add-on imported by multiple agents) into one
/// entry with a per-agent enabled state. Fail-open: an unreadable agent
/// profile is skipped (+ `tracing::warn`); a missing/unreadable agents dir
/// yields an empty list.
#[tauri::command]
pub fn addons_installed() -> Result<Vec<InstalledAddonAgg>, String> {
    let mur_home = crate::mur_home_path();
    let agents_dir = mur_home.join("agents");

    if !agents_dir.exists() {
        return Ok(vec![]);
    }

    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %agents_dir.display(), error = %e, "addons_installed: cannot read agents dir");
            return Ok(vec![]);
        }
    };

    let mut by_agent: std::collections::BTreeMap<String, Vec<mur_common::agent::AddonRef>> =
        std::collections::BTreeMap::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(agent_name) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let profile_path = dir.join("profile.yaml");
        match std::fs::read(&profile_path) {
            Ok(bytes) => match serde_yaml_ng::from_slice::<mur_common::AgentProfile>(&bytes) {
                Ok(profile) => {
                    by_agent.insert(agent_name.to_string(), profile.addons.clone());
                }
                Err(e) => {
                    tracing::warn!(agent = %agent_name, error = %e, "addons_installed: skipping unparsable profile");
                }
            },
            Err(e) => {
                tracing::warn!(agent = %agent_name, error = %e, "addons_installed: skipping unreadable profile");
            }
        }
    }

    Ok(aggregate_addons(&by_agent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_target_absolute_is_used_as_is() {
        let home = Path::new("/Users/x/.mur");
        assert_eq!(
            resolve_reveal_target("/abs/p", Some("a"), home),
            PathBuf::from("/abs/p")
        );
    }

    #[test]
    fn reveal_target_relative_resolves_under_agent() {
        let home = Path::new("/Users/x/.mur");
        assert_eq!(
            resolve_reveal_target("skills/ctx", Some("rustsmith"), home),
            PathBuf::from("/Users/x/.mur/agents/rustsmith/skills/ctx")
        );
    }

    #[test]
    fn reveal_target_relative_without_agent_under_home() {
        let home = Path::new("/Users/x/.mur");
        assert_eq!(
            resolve_reveal_target("models.yaml", None, home),
            PathBuf::from("/Users/x/.mur/models.yaml")
        );
    }

    fn stdio_entry(name: &str) -> mur_common::agent::McpServerEntry {
        mur_common::agent::McpServerEntry {
            name: name.to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), name.to_string()],
            ..Default::default()
        }
    }

    fn remote_entry(name: &str, url: &str) -> mur_common::agent::McpServerEntry {
        mur_common::agent::McpServerEntry {
            name: name.to_string(),
            url: Some(url.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn aggregate_shared_server_lists_both_agents_once() {
        let mut by_agent = std::collections::BTreeMap::new();
        by_agent.insert("a1".to_string(), vec![stdio_entry("shared")]);
        by_agent.insert("a2".to_string(), vec![stdio_entry("shared")]);

        let views = aggregate_mcp_servers(&by_agent);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].name, "shared");
        assert_eq!(views[0].transport, "stdio");
        assert_eq!(views[0].agents, vec!["a1".to_string(), "a2".to_string()]);
    }

    #[test]
    fn aggregate_distinct_servers_produce_distinct_entries() {
        let mut by_agent = std::collections::BTreeMap::new();
        by_agent.insert(
            "a1".to_string(),
            vec![
                stdio_entry("local-one"),
                remote_entry("remote-one", "https://example.com/mcp"),
            ],
        );

        let views = aggregate_mcp_servers(&by_agent);
        assert_eq!(views.len(), 2);
        let remote = views.iter().find(|v| v.name == "remote-one").unwrap();
        assert_eq!(remote.transport, "remote");
        assert_eq!(remote.description, "https://example.com/mcp");
        assert_eq!(remote.agents, vec!["a1".to_string()]);
    }

    #[test]
    fn aggregate_empty_map_returns_empty_list() {
        let by_agent = std::collections::BTreeMap::new();
        assert!(aggregate_mcp_servers(&by_agent).is_empty());
    }

    fn addon(id: &str, enabled: bool) -> mur_common::agent::AddonRef {
        mur_common::agent::AddonRef {
            id: id.to_string(),
            source: format!("claude-local:{id}@1.0.0"),
            enabled,
            skills: vec!["s1".to_string()],
            mcp: vec!["m1".to_string(), "m2".to_string()],
            commands: vec![],
        }
    }

    #[test]
    fn aggregate_addons_shared_across_agents_one_entry_per_agent_state() {
        let mut by_agent = std::collections::BTreeMap::new();
        by_agent.insert("a1".to_string(), vec![addon("superpowers", true)]);
        by_agent.insert("a2".to_string(), vec![addon("superpowers", false)]);

        let views = aggregate_addons(&by_agent);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].id, "superpowers");
        assert_eq!(views[0].skill_count, 1);
        assert_eq!(views[0].mcp_count, 2);
        assert_eq!(views[0].command_count, 0);
        assert_eq!(
            views[0].agents,
            vec![
                AddonAgentState {
                    agent: "a1".to_string(),
                    enabled: true
                },
                AddonAgentState {
                    agent: "a2".to_string(),
                    enabled: false
                },
            ]
        );
    }

    #[test]
    fn aggregate_addons_distinct_ids_produce_distinct_entries() {
        let mut by_agent = std::collections::BTreeMap::new();
        by_agent.insert(
            "a1".to_string(),
            vec![addon("superpowers", true), addon("quill", false)],
        );

        let views = aggregate_addons(&by_agent);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].id, "quill");
        assert_eq!(views[1].id, "superpowers");
    }

    #[test]
    fn aggregate_addons_empty_map_returns_empty_list() {
        let by_agent = std::collections::BTreeMap::new();
        assert!(aggregate_addons(&by_agent).is_empty());
    }
}
