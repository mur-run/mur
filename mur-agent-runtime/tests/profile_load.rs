use mur_agent_runtime::profile::{Profile, ProfileLoadError};
use std::fs;
use tempfile::TempDir;

fn minimal_yaml() -> String {
    r#"
schema: 1
id: "0192f5a1-28ab-7111-8000-000000000001"
name: agent_x
display_name: "X"
version: "0.1.0"
persona:
  category: research
  description: "X"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "m", params: {} }
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: { enabled: true, bind: "unix://{{agent_home}}/agent.sock" }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: ["a2a.message.send","a2a.tasks"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem:
    read: ["{{agent_home}}"]
    write: ["{{agent_home}}/workdir"]
    deny: []
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: ["rate_limit"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-22T10:00:00+08:00"
"#.to_string()
}

#[test]
fn load_expands_agent_home_template() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_x");
    fs::create_dir_all(&agent_home).unwrap();
    fs::write(agent_home.join("profile.yaml"), minimal_yaml()).unwrap();
    let profile = Profile::load(&agent_home).expect("load ok");
    let expected = format!("unix://{}/agent.sock", agent_home.display());
    assert_eq!(profile.inner.transport.socket.bind, expected);
    assert_eq!(profile.inner.entitlements.filesystem.read[0], agent_home.to_string_lossy());
}

#[test]
fn load_rejects_mismatched_name() {
    // Not yet implemented in this task but reserved for Task 4 spoof check.
    // Placeholder: load() itself does NOT compare argv[0] — that is done in multi_call.
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_x");
    fs::create_dir_all(&agent_home).unwrap();
    fs::write(agent_home.join("profile.yaml"), minimal_yaml()).unwrap();
    assert!(Profile::load(&agent_home).is_ok());
}

#[test]
fn digest_stable_across_reloads() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_x");
    fs::create_dir_all(&agent_home).unwrap();
    fs::write(agent_home.join("profile.yaml"), minimal_yaml()).unwrap();
    let p1 = Profile::load(&agent_home).unwrap();
    let p2 = Profile::load(&agent_home).unwrap();
    assert_eq!(p1.digest, p2.digest);
    assert!(p1.digest.starts_with("sha256:"));
}

#[test]
fn missing_profile_errors_cleanly() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("missing");
    match Profile::load(&agent_home) {
        Err(ProfileLoadError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}
