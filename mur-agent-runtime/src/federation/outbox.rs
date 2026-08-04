use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::{
    Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, Signal, SignalKind, SignalTarget,
};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Per-agent evidence outbox at `~/.mur/agents/<name>/outbox/`.
///
/// Agents write evidence signals here; `mur agent reconnect` flushes them
/// into the daemon's inbox so the Curator can apply them to canonical patterns.
pub struct AgentOutbox {
    dir: PathBuf,
    agent_name: String,
}

impl AgentOutbox {
    /// Open the outbox for `agent_name` (creates directory if missing).
    pub fn open(agent_name: &str) -> Result<Self> {
        let dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home dir"))?
            .join(".mur/agents")
            .join(agent_name)
            .join("outbox");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create outbox dir for {agent_name}"))?;
        Ok(Self {
            dir,
            agent_name: agent_name.to_owned(),
        })
    }

    /// Write an `ExecutionSuccess` signal for `pattern_name`.
    pub fn record_success(&self, pattern_name: &str) -> Result<PathBuf> {
        self.write_signal(pattern_name, SignalKind::ExecutionSuccess)
    }

    /// Write an `ExecutionFailure` signal for `pattern_name`.
    pub fn record_failure(&self, pattern_name: &str, error: &str) -> Result<PathBuf> {
        self.write_signal(
            pattern_name,
            SignalKind::ExecutionFailure {
                error: error.to_owned(),
            },
        )
    }

    /// Write a `UserOverrideAtBreakpoint` signal for `pattern_name`.
    pub fn record_override(&self, pattern_name: &str, reason: Option<&str>) -> Result<PathBuf> {
        self.write_signal(
            pattern_name,
            SignalKind::UserOverrideAtBreakpoint {
                reason: reason.map(|r| r.to_owned()),
            },
        )
    }

    /// List pending signal files, sorted chronologically (by filename prefix).
    pub fn list_pending(&self) -> Result<Vec<PathBuf>> {
        let mut items: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
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

    /// Move a flushed file into `.flushed/` for audit (caller decides retention).
    pub fn mark_flushed(&self, path: &Path) -> Result<()> {
        let flushed_dir = self.dir.join(".flushed");
        std::fs::create_dir_all(&flushed_dir)?;
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("path has no filename: {}", path.display()))?;
        std::fs::rename(path, flushed_dir.join(name))?;
        Ok(())
    }

    fn write_signal(&self, pattern_name: &str, kind: SignalKind) -> Result<PathBuf> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let signal = Signal {
            id,
            emitted_at: now,
            actor: Actor {
                source: ActorSource::MurCli,
                native_id: self.agent_name.clone(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: pattern_name.to_owned(),
                scope: Scope::Personal,
            },
            kind,
            scope: Scope::Personal,
            confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
            // Unsigned at rest in the agent's own outbox; the sleep-cycle
            // flush signs at the home boundary (federation/sync.rs, P2c-2).
            sig: None,
            key_version: 0,
        };

        let ts = now.format("%Y-%m-%dT%H-%M-%S");
        let fname = format!("{ts}-{id}.yaml");
        let dest = self.dir.join(&fname);
        let tmp = self.dir.join(format!(".{fname}.tmp"));
        let yaml =
            serde_yaml_ng::to_string(&signal).with_context(|| format!("serialize signal {id}"))?;
        std::fs::write(&tmp, yaml)?;
        std::fs::rename(&tmp, &dest)?;
        Ok(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_in(tmp: &TempDir, name: &str) -> AgentOutbox {
        let dir = tmp.path().join(".mur/agents").join(name).join("outbox");
        std::fs::create_dir_all(&dir).unwrap();
        AgentOutbox {
            dir,
            agent_name: name.to_owned(),
        }
    }

    #[test]
    fn record_success_creates_yaml() {
        let tmp = TempDir::new().unwrap();
        let ob = open_in(&tmp, "test-agent");
        let path = ob.record_success("rust-err-handling").unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let sig: Signal = serde_yaml_ng::from_str(&content).unwrap();
        assert!(matches!(sig.kind, SignalKind::ExecutionSuccess));
    }

    #[test]
    fn list_pending_sorted_chronological() {
        let tmp = TempDir::new().unwrap();
        let ob = open_in(&tmp, "test-agent");
        ob.record_success("p1").unwrap();
        ob.record_failure("p2", "timeout").unwrap();
        let pending = ob.list_pending().unwrap();
        assert_eq!(pending.len(), 2);
        // Filenames are timestamp-prefixed — sorted order = chronological.
        assert!(pending[0] < pending[1]);
    }

    #[test]
    fn mark_flushed_moves_file() {
        let tmp = TempDir::new().unwrap();
        let ob = open_in(&tmp, "test-agent");
        let path = ob.record_success("p1").unwrap();
        ob.mark_flushed(&path).unwrap();
        assert!(!path.exists());
        let flushed = ob.dir.join(".flushed");
        assert!(std::fs::read_dir(&flushed).unwrap().count() == 1);
    }
}
