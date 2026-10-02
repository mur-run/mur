use super::*;

/// Invoke a tool with `MUR_HOME` pointed at `mur_home` — the same
/// resolution path the running server uses (`resolve_mur_home`).
/// Like `call_tool_in`, for tools whose result is a JSON object.
async fn call_tool_json_in(
    mur_home: &std::path::Path,
    name: &str,
    arguments: Value,
) -> Result<Value, String> {
    let _env = mur_common::test_env::EnvGuard::set([("MUR_HOME", mur_home)]);
    dispatch_tool(name, &arguments).await
}

async fn call_tool_in(
    mur_home: &std::path::Path,
    name: &str,
    arguments: Value,
) -> Result<String, String> {
    let _env = mur_common::test_env::EnvGuard::set([("MUR_HOME", mur_home)]);
    let out = dispatch_tool(name, &arguments).await;
    match out? {
        Value::String(s) => Ok(s),
        other => Err(format!("expected a string tool result, got {other}")),
    }
}

/// The agent-facing half of the fix. Without this, a tool timeout leaves
/// the model with "outcome unknown" and nothing to ask — which is what
/// taught agents to re-dispatch work that was still in flight.
#[tokio::test]
async fn mur_job_status_reports_a_recorded_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    mur_core::run_status::store::save(
        mur_home,
        &mur_core::run_status::RunState {
            schema: mur_core::run_status::RUN_SCHEMA,
            run_id: "run-1".into(),
            channel_id: None,
            kind: mur_core::run_status::RunKind::Job,
            label: "two jobs".into(),
            pid: std::process::id(),
            started_at: chrono::Utc::now(),
            last_heartbeat_at: Some(chrono::Utc::now()),
            state: mur_core::run_status::State::Running,
            steps: vec![],
            blocked_on: None,
            binary_version: "0.0.0-test".into(),
            build_sha: "deadbee".into(),
        },
    )
    .unwrap();

    let out = call_tool_in(
        mur_home,
        "mur_job_status",
        serde_json::json!({ "run_id": "run-1" }),
    )
    .await
    .expect("tool call succeeded");

    assert!(out.contains("running"), "state missing from output: {out}");
    assert!(out.contains("alive"), "liveness missing from output: {out}");
}

/// A minimal valid profile for `name` whose only write grant is `write`.
fn write_member_profile(home: &std::path::Path, name: &str, write: &std::path::Path) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let yaml = format!(
        r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPF
name: {name}
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
  filesystem: {{ read: [], write: ['{write}'], deny: [] }}
  processes: {{ spawn: {{ mode: allowlist, allowed: [] }} }}
  syscalls: {{ mode: default }}
  limits: {{ memory_mb: 512, file_descriptors: 1024, processes: 32 }}
notifications: {{ on_task_complete: [], on_error: [], on_shutdown: [] }}
retry:
  llm: {{ max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout] }}
  tool: {{ max_retries: 1, backoff: fixed, initial_delay_ms: 500 }}
lifecycle: {{ restart: on_failure }}
created_at: "2026-04-29T10:00:00+00:00"
updated_at: "2026-04-29T10:00:00+00:00"
"#,
        // Single-quoted in YAML so a Windows `C:\Users\...` is not read as escapes.
        write = write.display().to_string().replace('\'', "''")
    );
    std::fs::write(dir.join("profile.yaml"), yaml).unwrap();
}

/// The tool answers with a handle, not with output: one job to an
/// authorized agent that is not running dispatches at once and fails on
/// its own afterwards.
#[tokio::test]
async fn parallel_jobs_returns_a_dispatch_handle() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("config.yaml"),
        "parallel_jobs:\n  targets: [ghost]\n",
    )
    .unwrap();
    // #1607: dispatch is gated on the member's write grant, so `ghost`
    // needs a profile that may already write the stated `cwd`.
    let project = std::fs::canonicalize(tmp.path()).unwrap().join("project");
    std::fs::create_dir_all(&project).unwrap();
    write_member_profile(tmp.path(), "ghost", &project);
    let t0 = std::time::Instant::now();
    let v = call_tool_json_in(
        tmp.path(),
        "parallel_jobs",
        serde_json::json!({
            "jobs": [{"description": "x", "agent": "ghost"}],
            "cwd": project.to_string_lossy(),
        }),
    )
    .await
    .unwrap();
    // The point is that dispatch returns a handle instead of waiting for
    // the job, not that it is fast: a contended CI runner can take seconds
    // to do the same non-blocking work. Keep the bound loose enough that
    // only an actual block — waiting on the ghost agent to fail, which
    // takes far longer — can trip it.
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(15),
        "dispatch blocked instead of returning a handle: {:?}",
        t0.elapsed()
    );
    assert_eq!(v["status"], "dispatched");
    assert!(v["run_id"].as_str().unwrap().starts_with("run-"), "{v}");
    assert!(
        v["follow"].as_str().unwrap().contains("mur_job_status"),
        "{v}"
    );
}

/// #1607: a bad `cwd` is refused at the tool boundary, before a channel
/// exists — the routing argument is validated, not just forwarded.
#[tokio::test]
async fn parallel_jobs_rejects_relative_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("config.yaml"),
        "parallel_jobs:\n  targets: [ghost]\n",
    )
    .unwrap();
    let err = call_tool_json_in(
        tmp.path(),
        "parallel_jobs",
        serde_json::json!({
            "jobs": [{"description": "x", "agent": "ghost"}],
            "cwd": "rel/dir"
        }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("absolute"), "{err}");
}

#[tokio::test]
async fn mur_job_status_appends_only_matching_fleet_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    mur_core::run_status::store::save(
        mur_home,
        &mur_core::run_status::RunState {
            schema: mur_core::run_status::RUN_SCHEMA,
            run_id: "run-fleet".into(),
            channel_id: None,
            kind: mur_core::run_status::RunKind::Fleet,
            label: "deep-research".into(),
            pid: std::process::id(),
            started_at: chrono::Utc::now(),
            last_heartbeat_at: Some(chrono::Utc::now()),
            state: mur_core::run_status::State::Running,
            steps: vec![],
            blocked_on: None,
            binary_version: "0.0.0-test".into(),
            build_sha: "deadbee".into(),
        },
    )
    .unwrap();
    let progress = mur_core::cmd::fleet::progress::RunProgress {
        schema_version: 1,
        run_id: "run-fleet".into(),
        question: "q".into(),
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        outcome: None,
        iteration: 2,
        model: Some("test-model".into()),
        budget_usd: None,
        spend_usd: 0.0,
        billable: Some(false),
        steps: vec![mur_core::cmd::fleet::progress::StepProgress {
            id: "s2".into(),
            worker: Some("worker".into()),
            phase: mur_core::cmd::fleet::progress::Phase::Verify,
            desc: "verify s2".into(),
            state: mur_core::cmd::fleet::progress::StepState::Running,
            cost_usd: None,
            started_at: None,
            ended_at: None,
        }],
        artifact_path: None,
        error: None,
    };
    progress.save(mur_home, "deep-research");

    let out = call_tool_in(
        mur_home,
        "mur_job_status",
        serde_json::json!({ "run_id": "run-fleet" }),
    )
    .await
    .unwrap();
    assert!(out.contains("progress: iteration 2"), "{out}");
    assert!(out.contains("running: verify s2"), "{out}");

    let mut other = progress;
    other.run_id = "run-other".into();
    other.save(mur_home, "deep-research");
    let out = call_tool_in(
        mur_home,
        "mur_job_status",
        serde_json::json!({ "run_id": "run-fleet" }),
    )
    .await
    .unwrap();
    assert!(!out.contains("progress:"), "{out}");
}

#[tokio::test]
async fn mur_job_status_on_an_unknown_run_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let out = call_tool_in(
        tmp.path(),
        "mur_job_status",
        serde_json::json!({ "run_id": "ghost" }),
    )
    .await
    .expect("tool call succeeded");
    assert!(
        out.contains("no run recorded"),
        "unhelpful miss message: {out}"
    );
}
