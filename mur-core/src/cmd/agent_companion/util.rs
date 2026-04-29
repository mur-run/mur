//! Shared helpers for `mur agent companion` subcommands.

use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Resolve the agent home directory for a given agent name.
pub(super) fn agent_home_for(name: &str) -> Result<PathBuf> {
    let mur_home = crate::paths::mur_root(None);
    let agent_dir = mur_home.join("agents").join(name);
    if !agent_dir.exists() {
        anyhow::bail!("agent {name} does not exist; run `mur agent create {name}` first");
    }
    Ok(agent_dir)
}

pub(super) fn atomic_write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let body = serde_yaml_ng::to_string(value).context("serialize yaml")?;
    atomic_write_bytes(path, body.as_bytes())
}

pub(super) fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let body = serde_json::to_string_pretty(value).context("serialize json")?;
    atomic_write_bytes(path, body.as_bytes())
}

pub(super) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("tmp")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}
