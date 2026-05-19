//! Agent-side sleep cycle: fires every `agent_idle_minutes`, flushes the
//! evidence outbox to `~/.mur/inbox/` (file-drop; daemon picks up), then
//! refreshes the snapshot via a `mur agent snapshot pull` subprocess.

use super::outbox::AgentOutbox;
use anyhow::Result;
use mur_common::Signal;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{info, warn};

/// Spawn a background task that periodically flushes the evidence outbox and
/// refreshes the pattern snapshot for `agent_name`.
pub fn spawn_agent_sleep_cycle(agent_name: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval_minutes = agent_idle_minutes();
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_minutes * 60));
        // Skip the immediate first tick — don't fire on startup.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) = run_agent_cycle(&agent_name) {
                warn!(agent = %agent_name, error = %e, "agent sleep-cycle error");
            }
        }
    })
}

fn run_agent_cycle(agent_name: &str) -> Result<()> {
    flush_outbox(agent_name)?;
    refresh_snapshot(agent_name);
    Ok(())
}

/// Copy pending outbox signals to `~/.mur/inbox/` for the daemon to pick up.
fn flush_outbox(agent_name: &str) -> Result<()> {
    let outbox = AgentOutbox::open(agent_name)?;
    let pending = outbox.list_pending()?;
    if pending.is_empty() {
        return Ok(());
    }

    let inbox_dir = mur_inbox_dir()?;
    std::fs::create_dir_all(&inbox_dir)?;

    let mut flushed = Vec::new();
    for path in &pending {
        let yaml = std::fs::read_to_string(path)?;
        let signal: Signal = serde_yaml_ng::from_str(&yaml)?;
        let fname = format!(
            "{}-{}.yaml",
            signal.emitted_at.format("%Y-%m-%dT%H-%M-%S"),
            signal.id
        );
        let dest = inbox_dir.join(&fname);
        let tmp = inbox_dir.join(format!(".{fname}.tmp"));
        std::fs::write(&tmp, &yaml)?;
        std::fs::rename(&tmp, &dest)?;
        flushed.push(path.clone());
    }

    info!(
        agent = %agent_name,
        count = flushed.len(),
        "agent sleep-cycle: outbox flushed to daemon inbox"
    );
    for path in &flushed {
        outbox.mark_flushed(path)?;
    }
    Ok(())
}

/// Invoke `mur agent snapshot pull <name>` as a best-effort subprocess.
fn refresh_snapshot(agent_name: &str) {
    let mur_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("mur")))
        .unwrap_or_else(|| PathBuf::from("mur"));
    let _ = std::process::Command::new(&mur_bin)
        .args(["agent", "snapshot", "pull", agent_name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    info!(agent = %agent_name, "agent sleep-cycle: snapshot pull triggered");
}

fn mur_inbox_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    Ok(home.join(".mur/inbox"))
}

fn agent_idle_minutes() -> u64 {
    // Read from config file; fall back to 5 min if config unreadable.
    let config_path = dirs::home_dir()
        .map(|h| h.join(".mur/config.yaml"))
        .unwrap_or_else(|| PathBuf::from(".mur/config.yaml"));
    if let Ok(yaml) = std::fs::read_to_string(&config_path)
        && let Ok(val) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&yaml)
        && let Some(minutes) = val
            .get("sleep_cycle")
            .and_then(|s| s.get("agent_idle_minutes"))
            .and_then(|m| m.as_u64())
    {
        return minutes;
    }
    5
}
