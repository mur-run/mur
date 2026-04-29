//! `mur agent companion voice eject|rebuild|diff` — write, re-compose, and diff
//! the agent's `companion/voice.md`.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use mur_agent_runtime::companion::voice::{VoiceInput, compose_in_memory};
use mur_common::agent::AgentProfile;
use std::path::Path;

use super::util::{agent_home_for, atomic_write_bytes};

// ─── CLI types ────────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct VoiceArgs {
    #[command(subcommand)]
    pub cmd: VoiceCmd,
}

#[derive(Subcommand, Debug)]
pub enum VoiceCmd {
    /// Compose voice.md from current profile + write to disk for editing.
    Eject {
        /// Agent name.
        name: String,
    },
    /// Re-compose voice.md from current profile, overwriting disk.
    Rebuild {
        /// Agent name.
        name: String,
        /// Override the freshness guard (refuses by default if disk file is newer than onboarding completed_at).
        #[arg(long)]
        force: bool,
    },
    /// Show unified diff between in-memory composed voice and the on-disk voice.md.
    Diff {
        /// Agent name.
        name: String,
    },
}

// ─── Entry point ──────────────────────────────────────────────────────────────

pub async fn run(args: VoiceArgs) -> Result<()> {
    match args.cmd {
        VoiceCmd::Eject { name } => {
            let agent_home = agent_home_for(&name)?;
            eject_at(&agent_home)
        }
        VoiceCmd::Rebuild { name, force } => {
            let agent_home = agent_home_for(&name)?;
            rebuild_at(&agent_home, force)
        }
        VoiceCmd::Diff { name } => {
            let agent_home = agent_home_for(&name)?;
            diff_at(&agent_home, &mut std::io::stdout())
        }
    }
}

// ─── Path-taking implementations (also used by tests) ─────────────────────────

pub(crate) fn eject_at(agent_home: &Path) -> Result<()> {
    let profile = load_profile(agent_home)?;
    let composed = compose_from_profile(&profile);
    let voice_dir = agent_home.join("companion");
    std::fs::create_dir_all(&voice_dir)?;
    let voice_path = voice_dir.join("voice.md");
    atomic_write_bytes(&voice_path, composed.as_bytes())?;
    println!(
        "✓ wrote {} ({} bytes)",
        voice_path.display(),
        composed.len()
    );
    Ok(())
}

pub(crate) fn rebuild_at(agent_home: &Path, force: bool) -> Result<()> {
    let profile = load_profile(agent_home)?;
    let voice_path = agent_home.join("companion/voice.md");

    if !force && voice_path.exists() {
        let onboarding_completed = profile.companion.onboarding.completed_at;
        let disk_mtime = std::fs::metadata(&voice_path)
            .with_context(|| format!("stat {}", voice_path.display()))?
            .modified()
            .context("read mtime")?;
        let disk_mtime_chrono: chrono::DateTime<chrono::Utc> = disk_mtime.into();
        if let Some(completed) = onboarding_completed
            && disk_mtime_chrono > completed
        {
            anyhow::bail!(
                "voice.md mtime ({}) is newer than onboarding completed_at ({}); \
                 user edits would be lost. Use --force to overwrite.",
                disk_mtime_chrono.to_rfc3339(),
                completed.to_rfc3339()
            );
        }
    }

    let composed = compose_from_profile(&profile);
    let voice_dir = agent_home.join("companion");
    std::fs::create_dir_all(&voice_dir)?;
    atomic_write_bytes(&voice_path, composed.as_bytes())?;
    println!("✓ rebuilt {}", voice_path.display());
    Ok(())
}

pub(crate) fn diff_at(agent_home: &Path, out: &mut impl std::io::Write) -> Result<()> {
    let profile = load_profile(agent_home)?;
    let composed = compose_from_profile(&profile);
    let voice_path = agent_home.join("companion/voice.md");
    let on_disk = std::fs::read_to_string(&voice_path)
        .with_context(|| format!("read {}", voice_path.display()))?;

    if composed == on_disk {
        writeln!(
            out,
            "(no differences — composed in-memory matches {})",
            voice_path.display()
        )?;
        return Ok(());
    }

    if let Ok(diff_output) = invoke_external_diff(&composed, &on_disk) {
        write!(out, "{diff_output}")?;
    } else {
        let mut buf = Vec::new();
        print_simple_diff(&composed, &on_disk, &mut buf)?;
        out.write_all(&buf)?;
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn load_profile(agent_home: &Path) -> Result<AgentProfile> {
    let profile_path = agent_home.join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", profile_path.display()))
}

fn compose_from_profile(profile: &AgentProfile) -> String {
    let formality = profile
        .companion
        .voice_overrides
        .formality
        .as_ref()
        .map(|f| format!("{f:?}").to_lowercase())
        .unwrap_or_default();
    let name_for_user = profile
        .companion
        .voice_overrides
        .name_for_user
        .as_deref()
        .unwrap_or("");
    let extra = profile
        .companion
        .voice_overrides
        .extra_instructions
        .as_deref()
        .unwrap_or("");
    compose_in_memory(VoiceInput {
        relationship: profile.companion.relationship.clone(),
        locale: &profile.companion.locale,
        name_for_user,
        formality: &formality,
        extra_instructions: extra,
    })
}

fn invoke_external_diff(a: &str, b: &str) -> Result<String> {
    use std::fs;
    let tmp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let path_a = tmp_dir.join(format!("mur_voice_diff_{pid}_a.md"));
    let path_b = tmp_dir.join(format!("mur_voice_diff_{pid}_b.md"));
    fs::write(&path_a, a.as_bytes())?;
    fs::write(&path_b, b.as_bytes())?;
    let result = std::process::Command::new("diff")
        .args(["-u", "--label", "composed", "--label", "disk"])
        .arg(&path_a)
        .arg(&path_b)
        .output();
    let _ = fs::remove_file(&path_a);
    let _ = fs::remove_file(&path_b);
    let out = result?;
    // diff exits 1 when files differ — that is expected and not an error here.
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn print_simple_diff(composed: &str, on_disk: &str, out: &mut impl std::io::Write) -> Result<()> {
    writeln!(out, "--- composed (in-memory)")?;
    writeln!(out, "+++ disk (companion/voice.md)")?;
    let composed_lines: Vec<&str> = composed.lines().collect();
    let disk_lines: Vec<&str> = on_disk.lines().collect();
    let max_len = composed_lines.len().max(disk_lines.len());
    for i in 0..max_len {
        let a = composed_lines.get(i).copied().unwrap_or("");
        let b = disk_lines.get(i).copied().unwrap_or("");
        if a != b {
            writeln!(out, "- [{i}] {a}")?;
            writeln!(out, "+ [{i}] {b}")?;
        }
    }
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Minimal profile YAML with companion enabled, locale, and relationship set.
    const MINIMAL_PROFILE_WITH_COMPANION: &str = r#"
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
companion:
  enabled: true
  locale: "en-US"
  relationship: friend
"#;

    fn write_minimal_profile_with_companion(dir: &Path) {
        std::fs::write(dir.join("profile.yaml"), MINIMAL_PROFILE_WITH_COMPANION).unwrap();
    }

    #[test]
    fn eject_writes_composed_voice_md() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_companion(tmp.path());

        eject_at(tmp.path()).unwrap();

        let voice_path = tmp.path().join("companion/voice.md");
        assert!(
            voice_path.exists(),
            "companion/voice.md must exist after eject"
        );
        let body = std::fs::read_to_string(&voice_path).unwrap();
        assert!(!body.is_empty(), "voice.md must not be empty");
    }

    #[test]
    fn rebuild_refuses_when_disk_newer_than_onboarding() {
        let tmp = TempDir::new().unwrap();

        // Write profile with onboarding.completed_at = 2 hours ago.
        let completed = chrono::Utc::now() - chrono::Duration::hours(2);
        let profile_yaml = format!(
            r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPF
name: test_agent
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
created_at: "2026-04-29T10:00:00+00:00"
updated_at: "2026-04-29T10:00:00+00:00"
companion:
  enabled: true
  locale: "en-US"
  relationship: friend
  onboarding:
    completed_at: "{}"
    version: 1
"#,
            completed.to_rfc3339()
        );
        std::fs::write(tmp.path().join("profile.yaml"), &profile_yaml).unwrap();

        // Write voice.md NOW (mtime = now, which is newer than completed_at = 2h ago).
        std::fs::create_dir_all(tmp.path().join("companion")).unwrap();
        std::fs::write(tmp.path().join("companion/voice.md"), "old content").unwrap();

        // rebuild without --force must fail.
        let err = rebuild_at(tmp.path(), false).unwrap_err();
        assert!(
            err.to_string().contains("user edits would be lost"),
            "expected freshness guard error, got: {err}"
        );

        // rebuild with --force must succeed and overwrite.
        rebuild_at(tmp.path(), true).unwrap();
        let body = std::fs::read_to_string(tmp.path().join("companion/voice.md")).unwrap();
        assert_ne!(body, "old content", "force rebuild must overwrite the file");
        assert!(!body.is_empty());
    }

    #[test]
    fn diff_returns_no_differences_when_in_sync() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_companion(tmp.path());

        // eject to write voice.md from composed content.
        eject_at(tmp.path()).unwrap();

        // diff must report no differences.
        let mut buf = Vec::new();
        diff_at(tmp.path(), &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("no differences"),
            "expected 'no differences', got: {output}"
        );
    }
}
