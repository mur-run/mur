//! Integration tests for `mur agent schedule` CLI commands.
//! Points MUR_HOME at a tempdir containing a minimal profile.yaml.
//!
//! MUR_HOME is a process-wide env var, so tests serialize via ENV_LOCK.

use mur_core::cmd::agent_schedule::{
    cmd_schedule_accept, cmd_schedule_add, cmd_schedule_list, cmd_schedule_next,
    cmd_schedule_remove, read_schedule,
};
use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;

/// Serialize all tests that mutate MUR_HOME to prevent cross-test interference.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard that removes MUR_HOME from the environment on drop,
/// preventing leakage to subsequent tests even if a test panics.
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
fn add_list_remove_roundtrip() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("sched_test");
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;

    // Add two entries
    cmd_schedule_add("sched_test", "0 9 * * 1-5", "morning brief", None, None).unwrap();
    cmd_schedule_add(
        "sched_test",
        "0 18 * * 1-5",
        "end of day",
        Some("other_agent".to_string()),
        None,
    )
    .unwrap();

    // read_schedule returns both
    let entries = read_schedule("sched_test").unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].cron, "0 9 * * 1-5");
    assert_eq!(entries[0].message, "morning brief");
    assert!(entries[0].sends_to.is_none());
    assert_eq!(entries[1].cron, "0 18 * * 1-5");
    assert_eq!(entries[1].message, "end of day");
    assert_eq!(entries[1].sends_to.as_deref(), Some("other_agent"));

    // Remove index 0 (morning brief)
    cmd_schedule_remove("sched_test", 0).unwrap();
    let entries = read_schedule("sched_test").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cron, "0 18 * * 1-5");

    // Remove the last entry
    cmd_schedule_remove("sched_test", 0).unwrap();
    let entries = read_schedule("sched_test").unwrap();
    assert!(entries.is_empty());
}

#[test]
fn remove_out_of_bounds_returns_err() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("sched_oob");
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;
    let result = cmd_schedule_remove("sched_oob", 0);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("index"));
}

#[test]
fn schedule_next_does_not_error() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("sched_next");
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;
    cmd_schedule_add("sched_next", "0 * * * *", "hourly ping", None, None).unwrap();
    // cmd_schedule_next prints to stdout — just verify it doesn't error.
    cmd_schedule_next("sched_next", 3).unwrap();
}

#[test]
fn add_invalid_cron_returns_err() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("sched_bad_cron");
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;
    let result = cmd_schedule_add("sched_bad_cron", "not a cron", "msg", None, None);
    assert!(result.is_err());
}

#[test]
fn schedule_list_does_not_error() {
    let tmp = setup("sched_list");
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;
    cmd_schedule_add("sched_list", "0 9 * * *", "daily check", None, None).unwrap();
    cmd_schedule_list("sched_list").unwrap();
}

/// A proposal that says "once" must not be granted as a perpetual schedule.
///
/// The bound is part of what the reviewer approved, and `accept` is the one
/// place it can be silently dropped — the same shape as #957, where a narrow
/// DTO written back over a full record erased the fields it did not know about.
#[test]
fn accept_carries_the_one_shot_bound_onto_the_entry() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = setup("sched_once");
    unsafe {
        std::env::set_var("MUR_HOME", tmp.path());
    }
    let _home_guard = MurHomeGuard;

    let dir = tmp
        .path()
        .join("agents")
        .join("sched_once")
        .join(mur_common::agent::SCHEDULE_PROPOSAL_DIR);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("deadbeef1234.yaml"),
        "cron: \"0 10 1 9 *\"\nmessage: breakfast\nasked_for: tomorrow at 10\nnot_after: \"2026-09-01T10:00:00+08:00\"\nproposed_at: \"2026-08-31T00:00:00+00:00\"\n",
    )
    .unwrap();

    cmd_schedule_accept("sched_once", "deadbeef1234").unwrap();

    let entries = read_schedule("sched_once").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].not_after.as_deref(),
        Some("2026-09-01T10:00:00+08:00"),
        "granting a proposal must grant what it said, bound included"
    );
}
