//! `mur agent companion proactive enable|disable` — mutate
//! `profile.yaml::companion.proactive.enabled`.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use std::path::Path;

use super::util::{agent_home_for, atomic_write_yaml};

#[derive(Args, Debug)]
pub struct ProactiveArgs {
    #[command(subcommand)]
    pub cmd: ProactiveCmd,
}

#[derive(Subcommand, Debug)]
pub enum ProactiveCmd {
    /// Allow proactive (companion-initiated) messages.
    Enable {
        /// Agent name.
        name: String,
    },
    /// Disable proactive messages (reactive replies still work).
    Disable {
        /// Agent name.
        name: String,
    },
}

pub async fn run(args: ProactiveArgs) -> Result<()> {
    match args.cmd {
        ProactiveCmd::Enable { name } => set_enabled(&name, true),
        ProactiveCmd::Disable { name } => set_enabled(&name, false),
    }
}

/// Public helper used by the GUI bridge (D5 / M5.6.2). Mirrors what
/// `mur agent companion proactive {enable|disable}` does, minus the
/// `println!` (callers that need a CLI-style notification can wrap).
pub fn set_enabled(name: &str, enabled: bool) -> Result<()> {
    let agent_home = agent_home_for(name)?;
    set_enabled_at(&agent_home.join("profile.yaml"), enabled)?;
    println!("companion.proactive.enabled = {enabled} (agent: {name})");
    Ok(())
}

fn set_enabled_at(profile_path: &Path, enabled: bool) -> Result<()> {
    let yaml = std::fs::read_to_string(profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&yaml)
        .with_context(|| format!("parse {}", profile_path.display()))?;
    profile.companion.proactive.enabled = enabled;
    profile.updated_at = chrono::Utc::now().to_rfc3339();
    atomic_write_yaml(profile_path, &profile)
        .with_context(|| format!("write {}", profile_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::AgentProfile;
    use std::path::Path;
    use tempfile::TempDir;

    const MINIMAL_PROFILE: &str = r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPF
name: test_agent
display_name: "Test"
version: "0.1.0"
persona:
  category: custom
  description: "Test agent"
  traits: { tone: neutral, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "llama3.2:3b", params: { temperature: 0.2, max_tokens: 4096 } }
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: { enabled: false, bind: "" }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: []
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: [] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout, connection_error] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: false }
created_at: "2026-04-29T10:00:00+00:00"
updated_at: "2026-04-29T10:00:00+00:00"
"#;

    fn write_minimal_profile(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("profile.yaml");
        std::fs::write(&path, MINIMAL_PROFILE).unwrap();
        path
    }

    #[test]
    fn enable_then_disable_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = write_minimal_profile(tmp.path());

        set_enabled_at(&path, true).unwrap();
        let yaml = std::fs::read_to_string(&path).unwrap();
        let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(p.companion.proactive.enabled);

        set_enabled_at(&path, false).unwrap();
        let yaml = std::fs::read_to_string(&path).unwrap();
        let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(!p.companion.proactive.enabled);
    }
}
