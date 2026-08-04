//! Agent-side sleep cycle: fires every `agent_idle_minutes`, flushes the
//! evidence outbox to `~/.mur/inbox/` (file-drop; daemon picks up), then
//! drops a signed snapshot request for the daemon to serve (federation P0).

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

/// Ask the daemon for a fresh knowledge snapshot by dropping a signed request
/// into `<mur_home>/inbox/snapshot-requests/`. The daemon — outside this
/// sandbox — verifies the signature against the agent's on-disk pubkey and
/// assembles the snapshot central-side; this side writes ONE small file and
/// never spawns anything. (The previous `mur agent snapshot pull` subprocess
/// required `mur` on the spawn allowlist — the entire CLI surface — and died
/// with EPERM under sandbox.) Uses the same `<mur_home>/inbox/...` write
/// grant the outbox flush above relies on.
fn refresh_snapshot(agent_name: &str) {
    let mur_home = match dirs::home_dir() {
        Some(h) => h.join(".mur"),
        None => {
            warn!(agent = %agent_name, "agent sleep-cycle: no home dir; skipping snapshot request");
            return;
        }
    };
    match write_snapshot_request_at(&mur_home, agent_name) {
        Ok(()) => info!(agent = %agent_name, "agent sleep-cycle: snapshot request dropped"),
        Err(e) => {
            warn!(agent = %agent_name, error = %e, "agent sleep-cycle: snapshot request write failed")
        }
    }
}

/// Testable core of `refresh_snapshot`: sign with the agent identity and
/// atomically drop the request file.
fn write_snapshot_request_at(mur_home: &std::path::Path, agent_name: &str) -> Result<()> {
    use mur_common::snapshot_request::{SNAPSHOT_REQUEST_DIR, SnapshotRequest};
    let identity =
        mur_common::identity::AgentIdentity::load(&mur_home.join("agents").join(agent_name))
            .map_err(|e| anyhow::anyhow!("load agent identity: {e}"))?;
    let req = SnapshotRequest::create(agent_name, &identity, chrono::Utc::now());
    let dir = mur_home.join(SNAPSHOT_REQUEST_DIR);
    std::fs::create_dir_all(&dir)?;
    // One pending request per agent: deterministic name, tmp+rename.
    let dest = dir.join(format!("{agent_name}.yaml"));
    let tmp = dir.join(format!(".{agent_name}.yaml.tmp"));
    std::fs::write(&tmp, serde_yaml_ng::to_string(&req)?)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_request_is_written_and_verifies() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let agent_dir = home.join("agents/w1");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let id = mur_common::identity::AgentIdentity::generate();
        id.save(&agent_dir).unwrap();

        write_snapshot_request_at(home, "w1").unwrap();

        let p = home.join("inbox/snapshot-requests/w1.yaml");
        let req: mur_common::snapshot_request::SnapshotRequest =
            serde_yaml_ng::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert!(req.verify(&id.verifying_key_bytes()));
        assert_eq!(req.agent, "w1");
    }
}
