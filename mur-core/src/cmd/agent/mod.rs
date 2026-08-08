//! `mur agent` subcommand dispatchers — split per resource:
//!   - lifecycle (create/list/status/stop/remove/rename)
//!   - comm      (send/card)
//!   - service   (install-service)
//!   - stats     (stats/logs)
//!   - export    (cmd_export)
//!   - perm      (cmd_perm_*)
//!   - mcp       (cmd_mcp_*, McpAddPin)
//!   - skill     (cmd_skill_*)
//!   - prompt    (cmd_prompt_*)
//!   - secret    (cmd_secret_*)
//!
//! This module re-exports the public entry points so `crate::cmd::agent::*`
//! continues to resolve from main.rs and `crate::agent_admin`.
//!
//! P0a: just `create`.
// TODO(Q-B): `mur agent rekey <name>` — regenerate identity keypair and
// re-register with commander. Blocked on user decision in spec § 13.
// If accepted, keep UUID stable; only rotate pubkey + notify peers.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use mur_common::{AgentProfile as _AgentProfile, LockFile};

pub mod addon;
mod apply;
pub mod channel_import;
pub mod cli;
mod comm;
mod doctor;
mod effort;
pub mod export;
mod hub;
pub(crate) mod install;
pub mod lifecycle;
pub mod mcp;
pub mod mcp_discover;
pub mod mcp_login;
pub mod mcp_registry;
pub mod start;
mod who;
// Items exported for the Hub GUI (mur-hub-gui crate) — not used from the
// mur binary itself, so suppress dead_code for the binary target.
#[allow(dead_code)]
pub mod mcp_remote;
pub mod model_resolve;
mod peers;
mod perm;
mod prompt;
mod reconnect;
mod restart;
mod secret;
mod service;
pub mod skill;
pub mod skill_bundle;
pub mod skill_github;
pub mod skill_install_pack;
pub mod skill_registry_add;
pub mod skill_remote;
pub mod skill_signer_trust;
pub mod skill_verify;
mod snapshot;
pub mod stale;
pub mod stats;

// `pub use` re-exports for the public CLI dispatch API. The lib crate doesn't
// reference these names internally; consumers (main.rs, agent_admin) reach them
// via `crate::cmd::agent::cmd_*`. Rustc still flags them as `unused_imports`
// under the lib+bin compilation split — silenced here.
#[allow(unused_imports)]
pub use apply::cmd_agent_apply;
#[allow(unused_imports)]
pub use cli::cmd_cli;
pub use comm::{cmd_card, cmd_send};
#[allow(unused_imports)]
pub use doctor::cmd_doctor;
pub use effort::cmd_effort;
#[allow(unused_imports)]
pub use export::cmd_export;
#[allow(unused_imports)]
pub use hub::cmd_migrate_to_hub;
#[allow(unused_imports)]
pub use install::{cmd_inspect, cmd_install, cmd_uninstall};
#[allow(unused_imports)]
pub use lifecycle::{cmd_create, cmd_list, cmd_remove, cmd_rename, cmd_status, cmd_stop};
#[allow(unused_imports)]
pub use mcp::{
    McpAddPin, cmd_mcp_add, cmd_mcp_list, cmd_mcp_remove, cmd_mcp_rename, cmd_mcp_set_enabled,
    cmd_mcp_set_network,
};
#[allow(unused_imports)]
pub use peers::cmd_peers;
#[allow(unused_imports)]
pub use perm::{
    cmd_perm_allow_host, cmd_perm_allow_read, cmd_perm_allow_spawn, cmd_perm_allow_spawn_dir,
    cmd_perm_allow_write, cmd_perm_clear_tool, cmd_perm_deny_host, cmd_perm_deny_path,
    cmd_perm_deny_spawn, cmd_perm_deny_spawn_dir, cmd_perm_list_hosts, cmd_perm_list_tools,
    cmd_perm_set_limit, cmd_perm_set_mode, cmd_perm_set_tool, cmd_perm_show,
};
#[allow(unused_imports)]
pub(crate) use prompt::prompt_path_for;
#[allow(unused_imports)]
pub use prompt::{cmd_prompt_edit, cmd_prompt_set, cmd_prompt_show};
#[allow(unused_imports)]
pub use reconnect::cmd_agent_reconnect;
#[allow(unused_imports)]
pub use restart::{cmd_restart, restart_stale_excluding};
#[allow(unused_imports)]
pub use secret::{cmd_secret_delete, cmd_secret_list, cmd_secret_set};
#[allow(unused_imports)]
pub use service::cmd_install_service;
#[allow(unused_imports)]
pub use skill::{
    cmd_skill_add, cmd_skill_convert, cmd_skill_list, cmd_skill_remove, cmd_skill_set_enabled,
    cmd_skill_show,
};
#[allow(unused_imports)]
pub use snapshot::{cmd_snapshot_pull, cmd_snapshot_show};
pub use start::cmd_start;
#[allow(unused_imports)]
pub use stats::{cmd_logs, cmd_stats};
pub use who::cmd_who;
mod pending;
mod queue_cmd;
pub mod wizard;
#[allow(unused_imports)]
pub use pending::{cmd_pending_act, cmd_pending_list};
mod trash;
#[allow(unused_imports)]
pub use queue_cmd::{
    cmd_queue_cancel, cmd_queue_list, cmd_queue_pause, cmd_queue_resume, cmd_queue_retry,
};
#[allow(unused_imports)]
pub use trash::{cmd_trash_empty, cmd_trash_list, cmd_trash_now, cmd_trash_restore};

// ─── Shared helpers used across submodules ─────────────────────────

pub fn resolve_mur_home() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("MUR_HOME") {
        return Ok(PathBuf::from(v));
    }
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("no home dir"))?
        .join(".mur"))
}

pub(super) fn resolve_bin_dir() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("MUR_AGENT_BIN_DIR") {
        return Ok(PathBuf::from(v));
    }
    if let Some(home) = dirs::home_dir() {
        return Ok(home.join(".local/bin"));
    }
    bail!("cannot resolve bin dir")
}

pub(crate) fn resolve_runtime_target() -> PathBuf {
    if let Some(v) = std::env::var_os("MUR_AGENT_RUNTIME_BIN") {
        return PathBuf::from(v);
    }
    let runtime_filename = if cfg!(windows) {
        "mur-agent-runtime.exe"
    } else {
        "mur-agent-runtime"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bundle) = runtime_target_in_bundle(&exe, runtime_filename)
            && bundle.exists()
        {
            return bundle;
        }
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(runtime_filename);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from(runtime_filename)
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// If `exe` lives inside a macOS `.app` bundle's `Contents/MacOS` directory,
/// return the sibling runtime path. Returns `None` otherwise. Pure (testable).
pub(crate) fn runtime_target_in_bundle(
    exe: &std::path::Path,
    runtime_filename: &str,
) -> Option<PathBuf> {
    let dir = exe.parent()?;
    if dir.file_name().and_then(|s| s.to_str()) != Some("MacOS") {
        return None;
    }
    if dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        != Some("Contents")
    {
        return None;
    }
    Some(dir.join(runtime_filename))
}

/// Liveness probe for a pid. Delegates to the canonical implementation in
/// `mur_common::lock_file` so stop/remove/rename guards share the same logic
/// as classify().
pub(super) fn pid_alive(pid: u32) -> bool {
    mur_common::lock_file::pid_alive(pid)
}

pub(crate) fn load_profile_for_edit(name: &str) -> Result<(PathBuf, _AgentProfile)> {
    let mur_home = resolve_mur_home()?;
    let path = mur_home.join("agents").join(name).join("profile.yaml");
    if !path.exists() {
        bail!("agent '{name}' not found");
    }
    let yaml = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let profile: _AgentProfile =
        serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", path.display()))?;
    Ok((path, profile))
}

pub(crate) fn save_profile(path: &Path, profile: &mut _AgentProfile) -> Result<()> {
    // Fail-closed guard (#717): a profile save must never *introduce* a skill
    // ref that does not resolve to an installed skill under the agent dir.
    // Only newly-added refs (relative to the profile currently on disk) are
    // validated, so unrelated edits to a profile that already carries a
    // legacy dangling ref still save.
    if let Some(agent_dir) = path.parent() {
        let prior: std::collections::HashSet<String> = fs::read_to_string(path)
            .ok()
            .and_then(|y| serde_yaml_ng::from_str::<_AgentProfile>(&y).ok())
            .map(|p| p.skills.into_iter().collect())
            .unwrap_or_default();
        let added: Vec<String> = profile
            .skills
            .iter()
            .filter(|s| !prior.contains(s.as_str()))
            .cloned()
            .collect();
        skill::validate_skill_refs(agent_dir, &added)?;
    }

    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let yaml = serde_yaml_ng::to_string(profile).context("serialize profile.yaml")?;
    write_atomic(path, yaml.as_bytes())?;

    // Version gate: when the agents git repo is active, commit this change.
    // Best-effort — a commit failure never fails the primary save.
    if let Some(agent_dir) = path.parent()
        && let Some(agents_root) = agent_dir.parent()
        && let Some(agent_name) = agent_dir.file_name().and_then(|n| n.to_str())
        && agents_root.join(".git").exists()
    {
        match crate::store::versioned::agent::VersionedAgentStore::open(agents_root) {
            Ok(mut vs) => {
                if let Err(e) = vs.commit_existing_profile(agent_name, "updated") {
                    tracing::warn!(
                        agent = %agent_name,
                        error = %e,
                        "versioned agent commit failed"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to open versioned agent store for commit");
            }
        }
    }

    Ok(())
}

/// Used by stop/remove/rename to refuse mutation when the supervisor is alive.
pub(super) fn refuse_if_running(agent_home: &Path, name: &str) -> Result<()> {
    let lock_path = agent_home.join("running.lock");
    if !lock_path.exists() {
        return Ok(());
    }
    let bytes = fs::read(&lock_path).with_context(|| format!("read {}", lock_path.display()))?;
    if let Ok(lock) = serde_json::from_slice::<LockFile>(&bytes)
        && pid_alive(lock.pid)
    {
        bail!("agent '{name}' is running; stop it first");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn bundle_runtime_resolves_from_macos_dir() {
        // Hub exe inside a .app → runtime sibling in Contents/MacOS.
        let exe = Path::new("/Applications/MuR Hub.app/Contents/MacOS/mur-hub-gui");
        let got = runtime_target_in_bundle(exe, "mur-agent-runtime");
        assert_eq!(
            got.as_deref(),
            Some(Path::new(
                "/Applications/MuR Hub.app/Contents/MacOS/mur-agent-runtime"
            ))
        );
    }

    #[test]
    fn non_bundle_path_returns_none() {
        let exe = Path::new("/opt/homebrew/bin/mur");
        assert_eq!(runtime_target_in_bundle(exe, "mur-agent-runtime"), None);
    }
}
