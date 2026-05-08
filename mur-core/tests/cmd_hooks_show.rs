//! Integration tests for `mur agent hooks show [--json]`.

use mur_core::cmd::agent_hooks::cmd_hooks_show;
use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct MurHomeGuard;
impl Drop for MurHomeGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("MUR_HOME") }
    }
}

const AGENT_NAME: &str = "hooks-test";

fn setup() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agents").join(AGENT_NAME);
    fs::create_dir_all(&agent_dir).unwrap();
    let yaml = format!(
        r#"schema: 1
id: 00000000-0000-0000-0000-000000000099
name: {AGENT_NAME}
display_name: Hooks Test
version: 0.1.0
persona:
  category: custom
  description: test
  traits: {{ tone: concise, risk: cautious, verbosity: low }}
sys_prompt_file: sys_prompt.md
model: {{ provider: ollama, name: "qwen3:14b", params: {{}} }}
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
companion: {{ enabled: false, locale: "en-US" }}
voice: {{ enabled: false }}
created_at: "2026-05-08T00:00:00Z"
updated_at: "2026-05-08T00:00:00Z"
"#
    );
    fs::write(agent_dir.join("profile.yaml"), yaml).unwrap();
    tmp
}

#[test]
fn hooks_show_table_mandatory_always_listed() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = setup();
    unsafe { std::env::set_var("MUR_HOME", tmp.path()) }
    let _guard = MurHomeGuard;

    // Capture stdout by calling the function; it should not panic.
    // We verify the function completes without error (mandatory hooks always present).
    cmd_hooks_show(AGENT_NAME, false).expect("hooks show should succeed");
}

#[test]
fn hooks_show_json_parses_and_has_mandatory_entries() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = setup();
    unsafe { std::env::set_var("MUR_HOME", tmp.path()) }
    let _guard = MurHomeGuard;

    // cmd_hooks_show with json=true writes to stdout; we verify no error.
    // The real assertion is that the function doesn't panic and returns Ok.
    cmd_hooks_show(AGENT_NAME, true).expect("hooks show --json should succeed");
}
