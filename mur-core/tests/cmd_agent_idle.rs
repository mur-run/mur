//! Integration tests for `mur agent schedule idle-{add,list,remove}`.
//! Points MUR_HOME at a tempdir containing a minimal profile.yaml.
//!
//! MUR_HOME is a process-wide env var, so tests serialize via ENV_LOCK.

use mur_core::cmd::agent_schedule::{
    cmd_idle_add, cmd_idle_list, cmd_idle_remove, read_idle_triggers,
};
use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct MurHomeGuard;
impl Drop for MurHomeGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("MUR_HOME");
        }
    }
}

fn setup(agent: &str) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agents").join(agent);
    fs::create_dir_all(&agent_dir).unwrap();
    let yaml = format!(
        r#"schema: 1
id: 00000000-0000-0000-0000-000000000001
name: {agent}
display_name: {agent}
version: 0.1.0
persona:
  category: custom
  description: test
  traits: {{ tone: concise, risk: cautious, verbosity: low }}
sys_prompt_file: sys_prompt.md
model: {{ provider: ollama, name: "llama3.2:3b", params: {{}} }}
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: {{ enabled: false, bind: "unix:///tmp/test.sock" }}
communication: {{ accepts_from: ["*"], sends_to: [] }}
capabilities: []
entitlements:
  network:
    inbound: {{ ports: [] }}
    outbound: {{ mode: unrestricted, allow_hosts: [] }}
  filesystem: {{ read: [], write: [], deny: [] }}
  processes: {{ spawn: {{ mode: allowlist, allowed: [] }} }}
notifications: {{ on_task_complete: [], on_error: [], on_shutdown: [] }}
retry:
  llm: {{ max_retries: 3, backoff: exponential, initial_delay_ms: 1000, retry_on: [] }}
  tool: {{ max_retries: 1, backoff: fixed, initial_delay_ms: 500 }}
lifecycle:
  restart: on_failure
  max_restarts: 3
  restart_window_secs: 600
  stop_timeout_secs: 15
  mcp_required: false
  schedule: []
created_at: "2026-01-01T00:00:00+00:00"
updated_at: "2026-01-01T00:00:00+00:00"
"#
    );
    fs::write(agent_dir.join("profile.yaml"), yaml).unwrap();
    tmp
}

#[test]
fn idle_add_list_remove_roundtrip() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("idle_test");
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;

    cmd_idle_add("idle_test", 3600, "still there?", None, 600, true).unwrap();
    cmd_idle_add(
        "idle_test",
        86400,
        "daily check",
        Some("peer".to_string()),
        1800,
        false,
    )
    .unwrap();

    let entries = read_idle_triggers("idle_test").unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].after_secs, 3600);
    assert_eq!(entries[0].message, "still there?");
    assert!(entries[0].sends_to.is_none());
    assert!(entries[0].respect_quiet_hours);
    assert_eq!(entries[1].after_secs, 86400);
    assert_eq!(entries[1].message, "daily check");
    assert_eq!(entries[1].sends_to.as_deref(), Some("peer"));
    assert!(!entries[1].respect_quiet_hours);

    cmd_idle_remove("idle_test", 0).unwrap();
    let entries = read_idle_triggers("idle_test").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].after_secs, 86400);
}

#[test]
fn idle_remove_oob_returns_err() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("idle_oob");
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;
    let result = cmd_idle_remove("idle_oob", 0);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("index"));
}

#[test]
fn idle_add_zero_after_secs_returns_err() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("idle_zero");
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;
    let result = cmd_idle_add("idle_zero", 0, "msg", None, 600, true);
    assert!(result.is_err());
}

#[test]
fn idle_list_does_not_error() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("idle_list");
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;
    cmd_idle_add("idle_list", 60, "ping", None, 600, true).unwrap();
    cmd_idle_list("idle_list").unwrap();
}
