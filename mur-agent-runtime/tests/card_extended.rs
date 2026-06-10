use mur_agent_runtime::profile::Profile;
use mur_agent_runtime::protocol::a2a_server::MethodHandler;
use mur_agent_runtime::protocol::methods::card::CardHandler;
use std::sync::Arc;

fn test_profile() -> Profile {
    use tempfile::TempDir;
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("agent_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        include_str!("fixtures/card_full_profile.yaml"),
    )
    .unwrap();
    let p = Profile::load(&dir).unwrap();
    std::mem::forget(tmp);
    p
}

#[tokio::test]
async fn card_includes_pubkey_endpoints_deployment() {
    let p = Arc::new(test_profile());
    let handler = CardHandler::new(p);
    let json = handler
        .handle(
            None,
            &mur_agent_runtime::protocol::a2a_server::RequestContext::none(),
        )
        .await
        .unwrap();

    assert_eq!(json["pubkey"], "zTESTPUB");
    let eps = json["endpoints"].as_array().unwrap();
    // order: tcp first (most reachable), then unix-socket, then stdio
    assert_eq!(eps[0]["transport"], "tcp+noise");
    assert_eq!(eps[0]["reachability"], "lan");
    assert_eq!(json["deployment"]["type"], "docker");
    assert_eq!(json["deployment"]["environment"], "prod");
}

// M3.1: previous_pubkey + grace_expires_at exposed during grace
#[tokio::test]
async fn card_includes_previous_pubkey_during_grace() {
    use mur_agent_runtime::profile::Profile;
    use mur_agent_runtime::protocol::a2a_server::MethodHandler;
    use mur_agent_runtime::protocol::methods::card::CardHandler;
    use std::sync::Arc;
    use tempfile::TempDir;

    let yaml_with_grace = r#"
schema: 1
id: 0192f5a1-28ab-7111-8000-000000000003
name: agent_grace
display_name: "Grace"
version: "0.1.0"
persona:
  category: research
  description: "Agent in grace"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "m", params: {} }
mcp_servers: []
skills: []
identity:
  pubkey: zNEWKEY
  owner: t@example.com
  algorithm: ed25519
  key_version: 2
  previous_pubkey: zOLDKEY
  previous_key_version: 1
  grace_expires_at: "2099-04-25T10:00:00+08:00"
  rotated_at: "2026-04-25T10:00:00+08:00"
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
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: ["rate_limit"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-25T10:00:00+08:00"
"#;
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("agent_grace");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("profile.yaml"), yaml_with_grace).unwrap();
    let profile = Profile::load(&dir).unwrap();
    std::mem::forget(tmp);

    let handler = CardHandler::new(Arc::new(profile));
    let json = handler
        .handle(
            None,
            &mur_agent_runtime::protocol::a2a_server::RequestContext::none(),
        )
        .await
        .unwrap();
    assert_eq!(json["pubkey"], "zNEWKEY");
    assert_eq!(json["previous_pubkey"], "zOLDKEY");
    assert_eq!(json["previous_key_version"], 1);
    assert_eq!(json["key_version"], 2);
    assert_eq!(json["algorithm"], "ed25519");
    assert!(json.get("grace_expires_at").is_some());
}

#[tokio::test]
async fn card_omits_previous_pubkey_after_grace() {
    use mur_agent_runtime::profile::Profile;
    use mur_agent_runtime::protocol::a2a_server::MethodHandler;
    use mur_agent_runtime::protocol::methods::card::CardHandler;
    use std::sync::Arc;
    use tempfile::TempDir;

    let yaml_post_grace = r#"
schema: 1
id: 0192f5a1-28ab-7111-8000-000000000004
name: agent_post
display_name: "Post"
version: "0.1.0"
persona:
  category: research
  description: "Agent post-grace"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "m", params: {} }
mcp_servers: []
skills: []
identity:
  pubkey: zNEWKEY
  owner: t@example.com
  algorithm: ed25519
  key_version: 2
  previous_pubkey: zOLDKEY
  previous_key_version: 1
  grace_expires_at: "2020-01-01T00:00:00+00:00"
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
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: ["rate_limit"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-25T10:00:00+08:00"
"#;
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("agent_post");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("profile.yaml"), yaml_post_grace).unwrap();
    let profile = Profile::load(&dir).unwrap();
    std::mem::forget(tmp);

    let handler = CardHandler::new(Arc::new(profile));
    let json = handler
        .handle(
            None,
            &mur_agent_runtime::protocol::a2a_server::RequestContext::none(),
        )
        .await
        .unwrap();
    assert_eq!(json["pubkey"], "zNEWKEY");
    assert!(
        json.get("previous_pubkey").is_none(),
        "expired grace must omit previous_pubkey"
    );
    assert!(json.get("grace_expires_at").is_none());
}
