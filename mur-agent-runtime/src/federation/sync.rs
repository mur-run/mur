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
/// requests a fresh knowledge snapshot for `agent_name`.
///
/// `identity` is the supervisor's pre-sandbox-loaded keypair (supervisor step
/// 3a). It MUST be passed in rather than re-read from disk: the B1 sandbox
/// denies reads of the agent's own `identity.key` once applied, so a lazy
/// per-cycle `AgentIdentity::load` fails with "identity files not found" on
/// every enforcing platform — exactly what the T0 tracer caught live.
pub fn spawn_agent_sleep_cycle(
    agent_name: String,
    identity: std::sync::Arc<mur_common::identity::AgentIdentity>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval_minutes = agent_idle_minutes();
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_minutes * 60));
        // Skip the immediate first tick — don't fire on startup.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) = run_agent_cycle(&agent_name, &identity) {
                warn!(agent = %agent_name, error = %e, "agent sleep-cycle error");
            }
        }
    })
}

fn run_agent_cycle(agent_name: &str, identity: &mur_common::identity::AgentIdentity) -> Result<()> {
    flush_outbox(agent_name, identity)?;
    refresh_snapshot(agent_name, identity);
    Ok(())
}

/// Parse an outbox signal, sign it with the agent's identity (P2c-2), and
/// return the signed YAML to drop. Signing happens HERE — the trust boundary
/// where the signal leaves the agent's home — so every outbox writer is
/// covered without threading the key into each of them.
fn sign_signal_yaml(
    yaml: &str,
    identity: &mur_common::identity::AgentIdentity,
) -> Result<(Signal, String)> {
    let mut signal: Signal = serde_yaml_ng::from_str(yaml)?;
    signal.sign(identity);
    let signed = serde_yaml_ng::to_string(&signal)?;
    Ok((signal, signed))
}

/// Copy pending outbox signals to `~/.mur/inbox/` for the daemon to pick up,
/// signing each at the boundary so ingest can verify who said it.
fn flush_outbox(agent_name: &str, identity: &mur_common::identity::AgentIdentity) -> Result<()> {
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
        let (signal, signed_yaml) = sign_signal_yaml(&yaml, identity)?;
        let fname = format!(
            "{}-{}.yaml",
            signal.emitted_at.format("%Y-%m-%dT%H-%M-%S"),
            signal.id
        );
        let dest = inbox_dir.join(&fname);
        let tmp = inbox_dir.join(format!(".{fname}.tmp"));
        std::fs::write(&tmp, &signed_yaml)?;
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
fn refresh_snapshot(agent_name: &str, identity: &mur_common::identity::AgentIdentity) {
    let mur_home = match dirs::home_dir() {
        Some(h) => h.join(".mur"),
        None => {
            warn!(agent = %agent_name, "agent sleep-cycle: no home dir; skipping snapshot request");
            return;
        }
    };
    match write_snapshot_request_at(&mur_home, agent_name, identity) {
        Ok(()) => info!(agent = %agent_name, "agent sleep-cycle: snapshot request dropped"),
        Err(e) => {
            warn!(agent = %agent_name, error = %e, "agent sleep-cycle: snapshot request write failed")
        }
    }
}

/// Testable core of `refresh_snapshot`: sign with the (pre-sandbox-loaded)
/// agent identity and atomically drop the request file.
fn write_snapshot_request_at(
    mur_home: &std::path::Path,
    agent_name: &str,
    identity: &mur_common::identity::AgentIdentity,
) -> Result<()> {
    use mur_common::snapshot_request::{SNAPSHOT_REQUEST_DIR, SnapshotRequest};
    let req = SnapshotRequest::create(agent_name, identity, chrono::Utc::now());
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

        write_snapshot_request_at(home, "w1", &id).unwrap();

        let p = home.join("inbox/snapshot-requests/w1.yaml");
        let req: mur_common::snapshot_request::SnapshotRequest =
            serde_yaml_ng::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert!(req.verify(&id.verifying_key_bytes()));
        assert_eq!(req.agent, "w1");
    }

    #[test]
    fn flushed_signal_yaml_is_signed_and_verifies() {
        let id = mur_common::identity::AgentIdentity::generate();
        let unsigned = mur_common::Signal {
            id: uuid::Uuid::new_v4(),
            emitted_at: chrono::Utc::now(),
            actor: mur_common::Actor {
                source: mur_common::ActorSource::MurCli,
                native_id: "w1".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: mur_common::SignalTarget::Skill {
                name: "s".into(),
                scope: mur_common::Scope::Personal,
            },
            kind: mur_common::SignalKind::SkillExecutionSuccess,
            scope: mur_common::Scope::Personal,
            confidence: 1.0,
            schema_version: mur_common::SIGNAL_SCHEMA_VERSION,
            sig: None,
            key_version: 0,
        };
        let yaml = serde_yaml_ng::to_string(&unsigned).unwrap();
        let (signal, signed_yaml) = sign_signal_yaml(&yaml, &id).unwrap();
        assert!(signal.verify(&id.verifying_key_bytes()));
        // The DROPPED yaml (what ingest reads) carries the verifying signature.
        let reparsed: mur_common::Signal = serde_yaml_ng::from_str(&signed_yaml).unwrap();
        assert!(reparsed.verify(&id.verifying_key_bytes()));
    }
}
