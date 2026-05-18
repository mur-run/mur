//! `mur agent history/rollback` — versioned agent commands (E1 W3).

use crate::store::versioned::agent::VersionedAgentStore;
use anyhow::{Context, Result};
use std::path::Path;

use crate::store::yaml::default_mur_dir;

fn agents_dir() -> std::path::PathBuf {
    default_mur_dir().join("agents")
}

fn open_or_init(root: &Path) -> Result<VersionedAgentStore> {
    if root.join(".git").exists() {
        VersionedAgentStore::open(root)
            .with_context(|| format!("open versioned agent store at {}", root.display()))
    } else {
        VersionedAgentStore::init(root)
            .with_context(|| format!("init versioned agent store at {}", root.display()))
    }
}

// ── history ───────────────────────────────────────────────────────────────────

pub(crate) fn cmd_agent_history(name: &str) -> Result<()> {
    let root = agents_dir();
    let store = open_or_init(&root)?;

    let hist = store.history(name)?;
    if hist.is_empty() {
        println!("No version history for agent '{name}'.");
        println!("(History is recorded starting from the first save after versioned store init.)");
        return Ok(());
    }

    println!("History for agent '{name}':");
    println!("{:<5} {:<14} {:<26} reason", "v", "sha", "timestamp");
    println!("{}", "-".repeat(70));
    for e in &hist {
        let ts = chrono::DateTime::from_timestamp(e.timestamp, 0)
            .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        println!("{:<5} {:<14} {:<26} {}", e.version, e.sha, ts, e.reason);
    }
    Ok(())
}

// ── rollback ──────────────────────────────────────────────────────────────────

pub(crate) fn cmd_agent_rollback(name: &str, to: u32) -> Result<()> {
    let root = agents_dir();
    let mut store = open_or_init(&root)?;

    let current_v = store.current_version(name);
    if current_v == 0 {
        anyhow::bail!("Agent '{name}' has no version history — cannot rollback.");
    }
    if to >= current_v {
        anyhow::bail!(
            "Cannot rollback agent '{name}' to v{to}: current version is v{current_v}. \
             Choose a version less than v{current_v}."
        );
    }

    let rev = store.rollback_profile(name, to)?;
    println!(
        "Rolled back agent '{name}' profile to v{to} → new commit v{} ({})",
        rev.version, rev.sha
    );
    Ok(())
}
