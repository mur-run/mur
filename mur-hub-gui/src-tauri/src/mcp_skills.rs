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
}
