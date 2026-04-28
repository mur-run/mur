//! M6.1 — agent-side grace expiry cleanup.
//
// Uses `std::os::unix::fs::PermissionsExt::set_mode` which is Unix-only.
// Gate the whole file matching the pattern used by other Unix-only tests
// in this crate (sigterm_shutdown, long_path, etc.).
#![cfg(unix)]

use mur_agent_runtime::supervisor::grace_cleanup_if_expired;
use mur_common::agent::AgentProfile;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn write_min_profile(dir: &std::path::Path, key_version: u32, grace: Option<&str>) {
    let grace_line = grace
        .map(|g| format!("\n  grace_expires_at: \"{g}\""))
        .unwrap_or_default();
    let yaml = format!(
        r#"
schema: 1
id: 0192f5a1-28ab-7111-8000-000000000099
name: agent_grace
display_name: "Grace"
version: "0.1.0"
persona:
  category: research
  description: "Grace test"
  traits: {{ tone: concise, risk: cautious, verbosity: low }}
sys_prompt_file: "sys_prompt.md"
model: {{ provider: ollama, name: "m", params: {{}} }}
mcp_servers: []
skills: []
identity:
  pubkey: zNEWKEY
  algorithm: ed25519
  key_version: {key_version}
  previous_pubkey: zOLDKEY
  previous_key_version: 0{grace_line}
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
  llm: {{ max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: ["rate_limit"] }}
  tool: {{ max_retries: 1, backoff: fixed, initial_delay_ms: 500 }}
lifecycle: {{ restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }}
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-25T10:00:00+08:00"
"#
    );
    std::fs::write(dir.join("profile.yaml"), yaml).unwrap();
}

fn write_prev_files(dir: &std::path::Path) {
    std::fs::write(dir.join("identity.key.prev"), [0u8; 32]).unwrap();
    let mut perms = std::fs::metadata(dir.join("identity.key.prev"))
        .unwrap()
        .permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(dir.join("identity.key.prev"), perms).unwrap();
    std::fs::write(dir.join("identity.pub.prev"), b"zOLDKEY").unwrap();
}

#[test]
fn cleanup_runs_when_grace_expired() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write_min_profile(dir, 1, Some("2020-01-01T00:00:00+00:00"));
    write_prev_files(dir);

    let yaml = std::fs::read_to_string(dir.join("profile.yaml")).unwrap();
    let profile: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();

    grace_cleanup_if_expired(dir, &profile).expect("cleanup ok");

    assert!(
        !dir.join("identity.key.prev").exists(),
        "expired grace must shred identity.key.prev"
    );
    assert!(
        !dir.join("identity.pub.prev").exists(),
        "expired grace must remove identity.pub.prev"
    );

    let yaml = std::fs::read_to_string(dir.join("profile.yaml")).unwrap();
    assert!(
        !yaml.contains("previous_pubkey:"),
        "previous_pubkey must be cleared: {yaml}"
    );
    assert!(
        !yaml.contains("grace_expires_at:"),
        "grace_expires_at must be cleared: {yaml}"
    );
}

#[test]
fn cleanup_noop_when_grace_still_active() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write_min_profile(dir, 1, Some("2099-01-01T00:00:00+00:00"));
    write_prev_files(dir);

    let yaml = std::fs::read_to_string(dir.join("profile.yaml")).unwrap();
    let profile: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    grace_cleanup_if_expired(dir, &profile).unwrap();

    assert!(
        dir.join("identity.key.prev").exists(),
        "in-grace cleanup must NOT touch identity.key.prev"
    );
    assert!(dir.join("identity.pub.prev").exists());
}

#[test]
fn cleanup_noop_when_no_grace_field() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write_min_profile(dir, 0, None);

    let yaml = std::fs::read_to_string(dir.join("profile.yaml")).unwrap();
    let profile: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    grace_cleanup_if_expired(dir, &profile).unwrap();

    let yaml_after = std::fs::read_to_string(dir.join("profile.yaml")).unwrap();
    assert_eq!(
        yaml.trim(),
        yaml_after.trim(),
        "no-op when grace_expires_at absent"
    );
}
