//! Agent discovery — filesystem scan of `~/.mur/agents/*/agent.yaml`.
//!
//! Publishes a `Vec<AgentEntry>` snapshot every 5 seconds via a
//! `tokio::sync::watch` channel so Tauri backends can push `agents-updated`
//! events without JS-side polling.

use mur_common::{
    AgentProfile,
    agent::PersonaCategory,
    lock_file::{self, AgentStatusKind},
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::sync::watch;
use tokio::time::{Duration, interval};

/// Simplified agent status exposed to the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Running,
    Idle,
    Stale,
}

impl From<AgentStatusKind> for AgentStatus {
    fn from(kind: AgentStatusKind) -> Self {
        match kind {
            AgentStatusKind::Running => AgentStatus::Running,
            AgentStatusKind::Stopped => AgentStatus::Idle,
            AgentStatusKind::Stale => AgentStatus::Stale,
        }
    }
}

/// One row in the Hub agent list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentEntry {
    pub name: String,
    pub display_name: String,
    pub category: PersonaCategory,
    pub status: AgentStatus,
    pub model_id: String,
}

/// Background scanner that polls `$mur_home/agents/*/agent.yaml` at 5s intervals.
pub struct AgentDiscovery {
    mur_home: PathBuf,
    tx: watch::Sender<Vec<AgentEntry>>,
}

impl AgentDiscovery {
    /// Create a discovery instance and return the watch receiver.
    ///
    /// The caller should call `AgentDiscovery::run(self)` to start polling.
    pub fn new(mur_home: PathBuf) -> (Self, watch::Receiver<Vec<AgentEntry>>) {
        let initial = scan_agents(&mur_home);
        let (tx, rx) = watch::channel(initial);
        (AgentDiscovery { mur_home, tx }, rx)
    }

    /// Spawn a background task that re-scans every 5 seconds.
    pub fn run(self) {
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(5));
            ticker.tick().await; // consume the immediate first tick
            loop {
                ticker.tick().await;
                let entries = scan_agents(&self.mur_home);
                // send_if_modified avoids waking receivers when nothing changed
                let _ = self.tx.send_if_modified(|current| {
                    if *current != entries {
                        *current = entries;
                        true
                    } else {
                        false
                    }
                });
            }
        });
    }
}

/// Scan `$mur_home/agents/*/agent.yaml` and return current entries sorted by name.
pub fn scan_agents(mur_home: &Path) -> Vec<AgentEntry> {
    let agents_dir = mur_home.join("agents");
    let Ok(dir) = std::fs::read_dir(&agents_dir) else {
        return Vec::new();
    };

    let mut entries: Vec<AgentEntry> = dir
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|dir_entry| {
            let agent_dir = dir_entry.path();
            let yaml_path = agent_dir.join("agent.yaml");
            let bytes = std::fs::read(&yaml_path).ok()?;
            let profile: AgentProfile = serde_yaml_ng::from_slice(&bytes).ok()?;
            let lock_path = agent_dir.join("running.lock");
            let status = AgentStatus::from(lock_file::classify(&lock_path).kind);
            Some(AgentEntry {
                name: profile.name.clone(),
                display_name: profile.display_name.clone(),
                category: profile.persona.category,
                status,
                model_id: format!("{}/{}", profile.model.provider, profile.model.name),
            })
        })
        .collect();

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_agent_yaml(name: &str, display_name: &str, category: &str, model: &str) -> String {
        format!(
            r#"schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPE
name: "{name}"
display_name: "{display_name}"
version: "1.0.0"
persona:
  category: {category}
  description: "test agent"
  traits: {{ tone: concise, risk: cautious, verbosity: low }}
sys_prompt_file: "sys_prompt.md"
model: {{ provider: anthropic, name: "{model}" }}
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: {{ enabled: false, bind: "" }}
communication: {{ accepts_from: ["*"], sends_to: [] }}
capabilities: []
entitlements:
  network:
    inbound: {{ ports: [] }}
    outbound: {{ mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: {{ mode: system }} }}
  filesystem: {{ read: [], write: [], deny: [] }}
  processes: {{ spawn: {{ mode: allowlist, allowed: [] }} }}
  syscalls: {{ mode: default }}
  limits: {{ memory_mb: 512, file_descriptors: 1024, processes: 32 }}
notifications: {{ on_task_complete: [], on_error: [], on_shutdown: [] }}
retry:
  llm: {{ max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit] }}
  tool: {{ max_retries: 1, backoff: fixed, initial_delay_ms: 500 }}
lifecycle: {{ restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }}
created_at: "2026-01-01T00:00:00+00:00"
updated_at: "2026-01-01T00:00:00+00:00"
"#
        )
    }

    #[test]
    fn scan_returns_empty_for_missing_dir() {
        let dir = tempdir().unwrap();
        let result = scan_agents(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn scan_skips_malformed_yaml() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join("agents").join("bad-agent");
        fs::create_dir_all(&agents_dir).unwrap();
        fs::write(agents_dir.join("agent.yaml"), b"not: valid: yaml: [[[").unwrap();
        let result = scan_agents(dir.path());
        assert!(result.is_empty());
    }

    #[test]
    fn scan_returns_entry_for_valid_yaml() {
        let dir = tempdir().unwrap();
        let agent_dir = dir.path().join("agents").join("test-agent");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(
            agent_dir.join("agent.yaml"),
            make_agent_yaml(
                "test-agent",
                "Test Agent",
                "research",
                "claude-3-5-sonnet-20241022",
            ),
        )
        .unwrap();
        let result = scan_agents(dir.path());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "test-agent");
        assert_eq!(result[0].display_name, "Test Agent");
        assert_eq!(result[0].status, AgentStatus::Idle); // no lock file
    }

    #[test]
    fn status_running_when_lock_pid_alive() {
        let dir = tempdir().unwrap();
        let agent_dir = dir.path().join("agents").join("live-agent");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(
            agent_dir.join("agent.yaml"),
            make_agent_yaml(
                "live-agent",
                "Live Agent",
                "automation",
                "claude-3-5-sonnet-20241022",
            ),
        )
        .unwrap();
        // Write a complete lock file with the current process's PID (which is alive).
        let pid = std::process::id();
        let lock_json = format!(
            r#"{{"schema":1,"uuid":"01JQX4TM8Y9K7VQH6B2N3R5DPE","name":"live-agent","pid":{pid},"ppid":1,"started_at":"2026-01-01T00:00:00Z","binary_version":"2.0.0","transports":{{"stdio":true}},"card_digest":"","capabilities":[]}}"#
        );
        fs::write(agent_dir.join("running.lock"), lock_json).unwrap();
        let result = scan_agents(dir.path());
        assert_eq!(result[0].status, AgentStatus::Running);
    }
}
