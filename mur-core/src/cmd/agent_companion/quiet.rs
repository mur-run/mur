//! `mur agent companion quiet --for|--until|--off` — set or clear
//! `profile.yaml::companion.proactive.paused_until`.

use anyhow::{Context, Result};
use clap::Args;
use mur_agent_runtime::companion::telemetry::OutboxEvent;
use mur_agent_runtime::durable::ledger::Ledger;
use mur_common::agent::AgentProfile;
use std::path::Path;

use super::util::{agent_home_for, atomic_write_yaml};

#[derive(Args, Debug)]
pub struct QuietArgs {
    /// Agent name.
    pub name: String,
    /// Pause for a relative duration (e.g. "2h", "30m", "1d").
    #[arg(long = "for", conflicts_with_all = ["until", "off"])]
    pub for_: Option<String>,
    /// Pause until an absolute RFC 3339 timestamp.
    #[arg(long, conflicts_with_all = ["for_", "off"])]
    pub until: Option<String>,
    /// Clear any active pause.
    #[arg(long, conflicts_with_all = ["for_", "until"])]
    pub off: bool,
}

pub async fn run(args: QuietArgs) -> Result<()> {
    set(
        &args.name,
        args.for_.as_deref(),
        args.until.as_deref(),
        args.off,
    )
    .await
}

/// Public helper used by the GUI bridge (D5 / M5.6.2). Same semantics
/// as `run(QuietArgs)`, but takes individual fields so Tauri callers
/// don't need to construct a clap-derived struct. Exactly one of
/// `for_seconds_or_string` / `until` / `off=true` must be set.
///
/// `for_` accepts the same `<n>{s,m,h,d}` shorthand as the CLI flag
/// (e.g. "1h", "30m", "2d"). The GUI passes a literal seconds count
/// stringified as `<N>s` to keep the surface uniform.
pub async fn set(name: &str, for_: Option<&str>, until: Option<&str>, off: bool) -> Result<()> {
    let count = [for_.is_some(), until.is_some(), off]
        .iter()
        .filter(|x| **x)
        .count();
    if count != 1 {
        anyhow::bail!("exactly one of --for, --until, --off is required");
    }

    let agent_home = agent_home_for(name)?;
    let profile_path = agent_home.join("profile.yaml");
    let now = chrono::Utc::now();

    let new_paused_until: Option<chrono::DateTime<chrono::Utc>> = if off {
        None
    } else if let Some(s) = for_ {
        Some(now + parse_relative_duration(s)?)
    } else {
        let s = until.unwrap();
        let parsed = chrono::DateTime::parse_from_rfc3339(s)
            .with_context(|| format!("parse --until '{s}' as RFC 3339"))?;
        Some(parsed.with_timezone(&chrono::Utc))
    };

    apply_at(&profile_path, &agent_home, new_paused_until, now)?;

    match new_paused_until {
        Some(until) => println!(
            "companion.proactive.paused_until = {} (agent: {})",
            until.to_rfc3339(),
            name
        ),
        None => println!("companion.proactive.paused_until = cleared (agent: {name})"),
    }

    Ok(())
}

fn parse_relative_duration(s: &str) -> Result<chrono::Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration");
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let n: i64 = num_str
        .parse()
        .with_context(|| format!("parse number in '{s}'"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        other => anyhow::bail!("unknown duration unit '{other}'; expected s|m|h|d"),
    };
    Ok(chrono::Duration::seconds(secs))
}

pub(super) fn apply_at(
    profile_path: &Path,
    agent_home: &Path,
    new_paused_until: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let yaml = std::fs::read_to_string(profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: AgentProfile = serde_yaml_ng::from_str(&yaml)
        .with_context(|| format!("parse {}", profile_path.display()))?;
    profile.companion.proactive.paused_until = new_paused_until;
    profile.updated_at = now.to_rfc3339();
    atomic_write_yaml(profile_path, &profile)
        .with_context(|| format!("write {}", profile_path.display()))?;

    // Append QuietRequested to today's ledger.
    let ledger_dir = agent_home.join("companion/outbox-ledger");
    std::fs::create_dir_all(&ledger_dir)?;
    let mut ledger = Ledger::open(&ledger_dir)?;
    // For --off, the schema requires a DateTime<Utc>; use now as sentinel.
    let until_for_event = new_paused_until.unwrap_or(now);
    let event = OutboxEvent::QuietRequested {
        until: until_for_event,
        reason: if new_paused_until.is_some() {
            "user_requested".into()
        } else {
            "user_cleared".into()
        },
        at: now,
    };
    ledger.append(&event)?;

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

    fn write_minimal_profile(dir: &Path, _name: &str) -> std::path::PathBuf {
        let path = dir.join("profile.yaml");
        std::fs::write(&path, MINIMAL_PROFILE).unwrap();
        path
    }

    #[test]
    fn for_2h_sets_paused_until_2h_in_future() {
        let tmp = TempDir::new().unwrap();
        let profile_path = write_minimal_profile(tmp.path(), "test-agent");
        let agent_home = tmp.path();
        let now = chrono::Utc::now();
        let new_paused = Some(now + chrono::Duration::hours(2));

        apply_at(&profile_path, agent_home, new_paused, now).unwrap();

        let p: AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path).unwrap()).unwrap();
        let actual = p.companion.proactive.paused_until.unwrap();
        let expected = now + chrono::Duration::hours(2);
        let drift = (actual - expected).num_seconds().abs();
        assert!(drift <= 1, "drift = {drift}s");
    }

    #[test]
    fn off_clears_paused_until() {
        let tmp = TempDir::new().unwrap();
        let profile_path = write_minimal_profile(tmp.path(), "test-agent");
        let agent_home = tmp.path();

        // First set a pause.
        let now = chrono::Utc::now();
        apply_at(
            &profile_path,
            agent_home,
            Some(now + chrono::Duration::hours(2)),
            now,
        )
        .unwrap();

        // Now --off.
        apply_at(&profile_path, agent_home, None, now).unwrap();
        let p: AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path).unwrap()).unwrap();
        assert!(p.companion.proactive.paused_until.is_none());
    }
}
