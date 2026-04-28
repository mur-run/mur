//! Lifecycle operations for a single agent — status, stop, install
//! service. The GUI Status tab exposes these as Start / Stop / Restart
//! buttons.

use anyhow::{Context, Result, anyhow};
use mur_common::lock_file::AgentStatusKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::cmd::agent;

// ─── mutators ─────────────────────────────────────────────────────

pub fn stop(name: &str) -> Result<()> {
    agent::cmd_stop(name)
}

pub fn install_service(name: &str, dry_run: bool) -> Result<()> {
    agent::cmd_install_service(name, dry_run)
}

// ─── queries ──────────────────────────────────────────────────────

/// Typed view of an agent's running status. Mirrors what `mur agent
/// status <name>` prints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusView {
    pub name: String,
    pub agent_home: PathBuf,
    pub kind: String,
    pub pid: Option<u32>,
    pub uptime_seconds: Option<i64>,
    pub key_version: u32,
}

pub fn status(name: &str) -> Result<StatusView> {
    let mur_home = agent::resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        return Err(anyhow!("agent '{name}' not found"));
    }

    let lock_path = agent_home.join("running.lock");
    let status = mur_common::lock_file::classify(&lock_path);
    let kind = match status.kind {
        AgentStatusKind::Running => "running",
        AgentStatusKind::Stale => "stale",
        AgentStatusKind::Stopped => "stopped",
    }
    .to_string();

    let uptime_seconds = if status.kind == AgentStatusKind::Running {
        mur_common::lock_file::read(&lock_path)
            .ok()
            .flatten()
            .and_then(|lock| chrono::DateTime::parse_from_rfc3339(&lock.started_at).ok())
            .map(|start| {
                (chrono::Utc::now() - start.with_timezone(&chrono::Utc))
                    .num_seconds()
                    .max(0)
            })
    } else {
        None
    };

    // Pull key_version from profile.yaml.
    let profile_path = agent_home.join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&yaml).with_context(|| "parse profile.yaml")?;

    Ok(StatusView {
        name: name.to_string(),
        agent_home,
        kind,
        pid: status.pid,
        uptime_seconds,
        key_version: profile.identity.key_version,
    })
}
