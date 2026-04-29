//! `mur agent companion rhythm wipe <name>` — shred inbox, ledger, bandit
//! state, clear pause flags; preserve voice config.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use mur_agent_runtime::companion::telemetry::OutboxEvent;
use mur_agent_runtime::durable::ledger::Ledger;
use mur_common::agent::AgentProfile;
use std::path::Path;

use super::util::{agent_home_for, atomic_write_yaml};

// ── CLI types ──────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct RhythmArgs {
    #[command(subcommand)]
    pub cmd: RhythmCmd,
}

#[derive(Subcommand, Debug)]
pub enum RhythmCmd {
    /// Reset companion state (clears inbox, ledger, bandit, pause flags).
    /// Preserves voice config (relationship, locale, voice_overrides).
    Wipe {
        name: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

// ── Entry point ────────────────────────────────────────────────────────────

pub async fn run(args: RhythmArgs) -> Result<()> {
    match args.cmd {
        RhythmCmd::Wipe { name, yes } => {
            let agent_home = agent_home_for(&name)?;
            if !yes && !confirm_interactive(&agent_home)? {
                println!("aborted");
                return Ok(());
            }
            wipe_at(&agent_home)?;
            println!("✓ rhythm wiped for agent {name}");
            Ok(())
        }
    }
}

// ── Interactive confirmation ───────────────────────────────────────────────

fn confirm_interactive(agent_home: &Path) -> Result<bool> {
    use std::io::{BufRead, Write};
    let inbox = agent_home.join("companion/inbox");
    let ledger = agent_home.join("companion/outbox-ledger");
    let bandit = agent_home.join("companion/bandit-state.json");
    println!("This will permanently delete:");
    println!("  - {} ({} files)", inbox.display(), count_files(&inbox));
    println!("  - {} ({} files)", ledger.display(), count_files(&ledger));
    if bandit.exists() {
        println!("  - {}", bandit.display());
    }
    println!();
    println!("Voice config (relationship, locale, voice_overrides) will be preserved.");
    print!("Continue? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let answer = line.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

fn count_files(p: &Path) -> usize {
    if !p.exists() {
        return 0;
    }
    std::fs::read_dir(p).map(|it| it.count()).unwrap_or(0)
}

// ── Core wipe logic ────────────────────────────────────────────────────────

pub(super) fn wipe_at(agent_home: &Path) -> Result<()> {
    let companion_dir = agent_home.join("companion");
    let inbox = companion_dir.join("inbox");
    let ledger = companion_dir.join("outbox-ledger");
    let bandit = companion_dir.join("bandit-state.json");

    // Shred each file (best-effort) before removing the directories.
    if inbox.exists() {
        shred_dir(&inbox)?;
        let _ = std::fs::remove_dir_all(&inbox);
    }
    if ledger.exists() {
        shred_dir(&ledger)?;
        let _ = std::fs::remove_dir_all(&ledger);
    }
    if bandit.exists() {
        shred_file(&bandit)?;
    }

    // Clear paused_until and learning_until in profile.yaml.
    let profile_path = agent_home.join("profile.yaml");
    if profile_path.exists() {
        let body = std::fs::read_to_string(&profile_path)
            .with_context(|| format!("read {}", profile_path.display()))?;
        let mut profile: AgentProfile = serde_yaml_ng::from_str(&body)
            .with_context(|| format!("parse {}", profile_path.display()))?;
        profile.companion.proactive.paused_until = None;
        profile.companion.proactive.learning_until = None;
        profile.updated_at = chrono::Utc::now().to_rfc3339();
        atomic_write_yaml(&profile_path, &profile)
            .with_context(|| format!("write {}", profile_path.display()))?;
    }

    // Append RhythmWiped to a fresh ledger.
    std::fs::create_dir_all(&ledger)?;
    let mut new_ledger = Ledger::open(&ledger)?;
    new_ledger.append(&OutboxEvent::RhythmWiped {
        at: chrono::Utc::now(),
    })?;

    Ok(())
}

// ── Shred helpers ──────────────────────────────────────────────────────────

fn shred_dir(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let path = entry?.path();
        if path.is_file() {
            shred_file(&path)?;
        }
    }
    Ok(())
}

fn shred_file(path: &Path) -> Result<()> {
    // Try `shred -u` first (matches supervisor.rs::shred_file).
    let r = std::process::Command::new("shred")
        .arg("-u")
        .arg(path)
        .status();
    if let Ok(s) = r
        && s.success()
    {
        return Ok(());
    }
    // Fallback: overwrite with zeros then unlink.
    use std::io::Write;
    if let Ok(meta) = std::fs::metadata(path)
        && let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path)
    {
        let len = meta.len() as usize;
        let zeros = vec![0u8; len.max(32)];
        let _ = f.write_all(&zeros);
        let _ = f.sync_all();
    }
    let _ = std::fs::remove_file(path);
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_agent_runtime::companion::telemetry::OutboxEvent;
    use mur_agent_runtime::durable::ledger::Ledger;
    use mur_common::agent::AgentProfile;
    use tempfile::TempDir;

    /// A minimal valid profile YAML that includes companion fields used by the tests.
    /// Uses `en-US` locale, `Friend` relationship, `daily_cap: 5`.
    fn minimal_profile_yaml(paused: bool) -> String {
        let paused_line = if paused {
            let until = chrono::Utc::now() + chrono::Duration::hours(2);
            format!("    paused_until: \"{}\"", until.to_rfc3339())
        } else {
            String::new()
        };
        let learning_until = chrono::Utc::now() + chrono::Duration::days(7);
        format!(
            r#"schema: 1
id: test-id
name: test-agent
display_name: "Test"
version: "0.1.0"
persona:
  category: custom
  description: "Test agent"
  traits: {{ tone: neutral, risk: cautious, verbosity: low }}
sys_prompt_file: "sys_prompt.md"
model: {{ provider: ollama, name: "llama3.2:3b", params: {{ temperature: 0.2, max_tokens: 4096 }} }}
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
  llm: {{ max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout, connection_error] }}
  tool: {{ max_retries: 1, backoff: fixed, initial_delay_ms: 500 }}
lifecycle: {{ restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: false }}
companion:
  enabled: true
  locale: "en-US"
  relationship: friend
  proactive:
    enabled: true
    daily_cap: 5
    learning_until: "{}"
{}
created_at: "2026-04-29T10:00:00+00:00"
updated_at: "2026-04-29T10:00:00+00:00"
"#,
            learning_until.to_rfc3339(),
            paused_line
        )
    }

    fn write_minimal_profile_with_state(dir: &Path, paused: bool) {
        let profile_path = dir.join("profile.yaml");
        std::fs::write(&profile_path, minimal_profile_yaml(paused)).unwrap();
    }

    fn write_some_inbox(dir: &Path) {
        let inbox = dir.join("companion/inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("msg-001.md"), b"--- some inbox file ---").unwrap();
        std::fs::write(inbox.join("msg-002.md"), b"--- another file ---").unwrap();
    }

    fn write_some_ledger(dir: &Path) {
        let ledger_dir = dir.join("companion/outbox-ledger");
        std::fs::create_dir_all(&ledger_dir).unwrap();
        let mut l = Ledger::open(&ledger_dir).unwrap();
        l.append(&OutboxEvent::MessageScheduled {
            id: "x".into(),
            situation: mur_common::companion::Situation::MorningGreeting,
            template_id: "t".into(),
            scheduled_for: chrono::Utc::now(),
        })
        .unwrap();
    }

    #[test]
    fn wipe_removes_inbox_and_ledger_and_appends_rhythm_wiped() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_state(tmp.path(), true);
        write_some_inbox(tmp.path());
        write_some_ledger(tmp.path());

        wipe_at(tmp.path()).unwrap();

        // Inbox is gone.
        assert!(!tmp.path().join("companion/inbox").exists());

        // Ledger directory exists but only contains RhythmWiped.
        let ledger_dir = tmp.path().join("companion/outbox-ledger");
        assert!(ledger_dir.exists());
        let events: Vec<OutboxEvent> = Ledger::scan_days::<OutboxEvent>(&ledger_dir, 7)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], OutboxEvent::RhythmWiped { .. }));
    }

    #[test]
    fn wipe_clears_paused_and_learning_but_preserves_voice_config() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_state(tmp.path(), true);

        wipe_at(tmp.path()).unwrap();

        let body = std::fs::read_to_string(tmp.path().join("profile.yaml")).unwrap();
        let p: AgentProfile = serde_yaml_ng::from_str(&body).unwrap();
        assert!(p.companion.proactive.paused_until.is_none());
        assert!(p.companion.proactive.learning_until.is_none());
        // Preserved fields:
        assert!(p.companion.enabled);
        assert_eq!(p.companion.locale, "en-US");
        assert!(matches!(
            p.companion.relationship,
            mur_common::companion::Relationship::Friend
        ));
        // proactive.daily_cap should still be 5 (user-set behaviour preference).
        assert_eq!(p.companion.proactive.daily_cap, 5);
    }

    #[test]
    fn wipe_idempotent_on_empty_state() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_state(tmp.path(), false);
        wipe_at(tmp.path()).unwrap();
        wipe_at(tmp.path()).unwrap(); // second call should not error
    }
}
