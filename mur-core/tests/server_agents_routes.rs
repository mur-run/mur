//! End-to-end smoke for the read-only Phase 4 agent routes.
//! Spins up the real `build_router` against a tempdir, exercises every route.

#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use mur_core::server::{AppState, ServerConfig, build_router};

fn build_state(tmp: &tempfile::TempDir) -> AppState {
    let agents_dir = tmp.path().join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    AppState {
        patterns_dir: tmp.path().join("patterns"),
        workflows_dir: tmp.path().join("workflows"),
        pipelines_dir: tmp.path().join("pipelines"),
        agents_dir,
        index_dir: tmp.path().join("index"),
        config: ServerConfig { readonly: false },
        events_tx: tokio::sync::broadcast::channel(64).0,
    }
}

fn write_min_profile(dir: &std::path::Path, name: &str, display: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let id_suffix = name.bytes().fold(0u64, |a, b| a.wrapping_add(b as u64));
    let yaml = format!(
        "schema: 1\nid: 0192f5a1-28ab-7111-8000-{id_suffix:012x}\nname: {name}\n\
         display_name: \"{display}\"\nversion: \"0.1.0\"\npersona:\n  category: research\n  \
         description: \"Test agent\"\n  traits:\n    tone: concise\n    risk: cautious\n    verbosity: low\n\
         sys_prompt_file: \"sys_prompt.md\"\nmodel:\n  provider: ollama\n  name: \"m\"\n  params: {{}}\n\
         mcp_servers: []\nskills: []\ntransport:\n  stdio: true\n  socket:\n    enabled: true\n    bind: \"unix:///tmp/{name}.sock\"\n\
         communication:\n  accepts_from: [\"*\"]\n  sends_to: []\ncapabilities: [\"a2a.message.send\"]\n\
         entitlements:\n  network:\n    inbound:\n      ports: []\n    outbound:\n      mode: restricted\n      allow_hosts: []\n      protocols: [\"tcp\"]\n      resolve_dns:\n        mode: system\n  filesystem:\n    read: []\n    write: []\n    deny: []\n  processes:\n    spawn:\n      mode: allowlist\n      allowed: []\n  syscalls:\n    mode: default\n  limits:\n    memory_mb: 512\n    file_descriptors: 1024\n    processes: 32\n\
         notifications:\n  on_task_complete: []\n  on_error: []\n  on_shutdown: []\nretry:\n  llm:\n    max_retries: 3\n    backoff: exponential\n    initial_delay_ms: 1000\n    max_delay_ms: 30000\n    retry_on: [\"rate_limit\"]\n  tool:\n    max_retries: 1\n    backoff: fixed\n    initial_delay_ms: 500\n\
         lifecycle:\n  restart: on_failure\n  max_restarts: 3\n  restart_window_secs: 600\n  stop_timeout_secs: 15\n  mcp_required: true\n\
         created_at: \"2026-04-28T10:00:00+08:00\"\nupdated_at: \"2026-04-28T10:00:00+08:00\"\n"
    );
    std::fs::write(dir.join("profile.yaml"), yaml).unwrap();
}

#[tokio::test]
async fn end_to_end_all_readonly_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(&tmp);
    let home = state.agents_dir.join("support_bot");
    write_min_profile(&home, "support_bot", "Support");
    std::fs::create_dir_all(home.join("telemetry")).unwrap();
    std::fs::write(
        home.join("telemetry").join("2026-04-28.jsonl"),
        r#"{"ts":"2026-04-28T07:00:00Z","kind":"start"}
"#,
    )
    .unwrap();
    std::fs::create_dir_all(home.join("evals")).unwrap();
    std::fs::write(
        home.join("evals").join("eval-20260428T072501-aaaa.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "run_id": "eval-20260428T072501-aaaa",
            "suite": "v1",
            "agent": "support_bot",
            "started_at": "2026-04-28T07:25:01Z",
            "finished_at": "2026-04-28T07:25:05Z",
            "matrix": {"providers": ["ollama/llama3.2:3b"], "system_prompts": ["a"]},
            "results": [],
            "summary": {"total": 0, "passed": 0, "failed": 0}
        }))
        .unwrap(),
    )
    .unwrap();

    let app = build_router(state);
    for path in [
        "/api/v1/agents",
        "/api/v1/agents/support_bot",
        "/api/v1/agents/support_bot/telemetry?date=2026-04-28",
        "/api/v1/agents/support_bot/evals",
        "/api/v1/agents/support_bot/evals/eval-20260428T072501-aaaa",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "route {path} returned non-200"
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(!bytes.is_empty(), "route {path} returned empty body");
    }
}
