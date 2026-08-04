use anyhow::{Context, Result};
use mur_common::Signal;

/// Flush an agent's evidence outbox into the daemon inbox and apply to patterns.
///
/// Reads Signal YAML files from `~/.mur/agents/<name>/outbox/`, copies them
/// into `~/.mur/inbox/`, applies them via `Inbox::apply_all`, then marks
/// each successfully-ingested file as flushed.
pub fn cmd_agent_reconnect(name: &str) -> Result<()> {
    let outbox_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur/agents")
        .join(name)
        .join("outbox");

    if !outbox_dir.exists() {
        println!("No outbox found for agent '{name}' — nothing to reconnect.");
        return Ok(());
    }

    let pending = collect_pending(&outbox_dir)?;
    if pending.is_empty() {
        println!("Outbox for '{name}' is empty — already up to date.");
        return Ok(());
    }

    let inbox = crate::sync::inbox::Inbox::default_location()?;
    let store = crate::store::yaml::YamlStore::default_store()?;

    // Best-effort P2c-2 signing at the flush boundary, mirroring the runtime
    // sleep-cycle: the CLI runs as the user, who owns the agent's key. A
    // missing key just flushes unsigned (legacy-tolerated on ingest).
    let identity = outbox_dir
        .parent()
        .and_then(|agent_dir| mur_common::identity::AgentIdentity::load(agent_dir).ok());

    let mut received: Vec<std::path::PathBuf> = Vec::new();
    for path in &pending {
        let yaml = std::fs::read_to_string(path)
            .with_context(|| format!("read outbox file {}", path.display()))?;
        let mut signal: Signal = serde_yaml::from_str(&yaml)
            .with_context(|| format!("parse signal from {}", path.display()))?;
        if let Some(id) = &identity {
            signal.sign(id);
        }
        inbox.receive(&signal)?;
        received.push(path.clone());
    }

    let report = inbox.apply_all(&store)?;
    println!(
        "Reconnected '{name}': {} signals flushed, {} applied, {} skipped",
        pending.len(),
        report.applied,
        report.skipped
    );
    if !report.errors.is_empty() {
        for e in &report.errors {
            eprintln!("  warning: {e}");
        }
    }

    // Mark flushed *after* successful apply_all so we don't lose signals on error.
    let flushed_dir = outbox_dir.join(".flushed");
    std::fs::create_dir_all(&flushed_dir)?;
    for path in &received {
        let fname = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("no filename: {}", path.display()))?;
        std::fs::rename(path, flushed_dir.join(fname))?;
    }

    Ok(())
}

fn collect_pending(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut items: Vec<std::path::PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("yaml")
                && !p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with('.'))
                && p.is_file()
        })
        .collect();
    items.sort();
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, SignalKind, SignalTarget};
    use tempfile::TempDir;
    use uuid::Uuid;

    fn write_success_signal(dir: &std::path::Path, pattern: &str) {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now();
        let signal = Signal {
            id,
            emitted_at: now,
            actor: Actor {
                source: ActorSource::MurCli,
                native_id: "test-agent".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: pattern.to_owned(),
                scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal,
            confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
            sig: None,
            key_version: 0,
        };
        let ts = now.format("%Y-%m-%dT%H-%M-%S");
        let fname = format!("{ts}-{id}.yaml");
        let yaml = serde_yaml::to_string(&signal).unwrap();
        std::fs::write(dir.join(fname), yaml).unwrap();
    }

    #[test]
    fn collect_pending_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let pending = collect_pending(tmp.path()).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn collect_pending_skips_hidden_and_non_yaml() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".hidden.yaml"), "").unwrap();
        std::fs::write(tmp.path().join("real.txt"), "").unwrap();
        write_success_signal(tmp.path(), "p1");
        let pending = collect_pending(tmp.path()).unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn collect_pending_sorted() {
        let tmp = TempDir::new().unwrap();
        write_success_signal(tmp.path(), "p1");
        write_success_signal(tmp.path(), "p2");
        let pending = collect_pending(tmp.path()).unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending[0] < pending[1]);
    }
}
