//! `GET /api/v1/agents` — list agents under `~/.mur/agents/`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::server::{AppError, AppState};

#[derive(Serialize, Debug, Clone)]
pub struct AgentListEntry {
    pub name: String,
    pub display_name: String,
    pub status: super::AgentStatusKind,
}

pub async fn handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<AgentListEntry>>, AppError> {
    let entries = match std::fs::read_dir(&state.agents_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Json(Vec::new()));
        }
        Err(e) => {
            return Err(AppError::Internal(
                anyhow::Error::from(e).context(format!("read_dir {}", state.agents_dir.display())),
            ));
        }
    };

    let mut out: Vec<AgentListEntry> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let profile_path = path.join("profile.yaml");
        let yaml = match std::fs::read_to_string(&profile_path) {
            Ok(y) => y,
            Err(_) => continue, // not an agent dir
        };
        let profile: mur_common::AgentProfile = match serde_yaml_ng::from_str(&yaml) {
            Ok(p) => p,
            Err(_) => continue,
        };
        out.push(AgentListEntry {
            name: profile.name,
            display_name: profile.display_name,
            status: super::agent_status(&path).kind,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(out))
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::server::build_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    pub(crate) fn write_min_profile(dir: &std::path::Path, name: &str, display: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let id_suffix = name.bytes().fold(0u64, |a, b| a.wrapping_add(b as u64));
        // Use a minimal but valid profile YAML structure
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

    /// Build a minimal but valid `LockFile` with the given pid.
    pub(crate) fn make_lock(pid: u32, name: &str) -> mur_common::LockFile {
        mur_common::LockFile {
            schema: 1,
            uuid: "0192f5a1-28ab-7111-8000-000000000001".to_string(),
            name: name.to_string(),
            pid,
            ppid: 1,
            started_at: "2026-04-28T10:00:00Z".to_string(),
            binary_version: "0.1.0".to_string(),
            transports: mur_common::agent::LockTransports {
                stdio: true,
                unix_socket: None,
                tcp: None,
                webhook: None,
            },
            card_digest: "sha256:abc123".to_string(),
            capabilities: vec!["a2a.message.send".to_string()],
            build_sha: String::new(),
            proto_version: 0,
            sandbox: None,
        }
    }

    #[tokio::test]
    async fn list_returns_known_agents_with_status() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join("agents");
        write_min_profile(&agents_dir.join("alpha"), "alpha", "Alpha Bot");
        write_min_profile(&agents_dir.join("beta"), "beta", "Beta Bot");
        // alpha is "running" — write a real LockFile JSON using the current process's pid.
        let lock = make_lock(std::process::id(), "alpha");
        std::fs::write(
            agents_dir.join("alpha").join("running.lock"),
            serde_json::to_vec_pretty(&lock).unwrap(),
        )
        .unwrap();

        // Build state pointing at this temp agents dir
        let state = crate::server::AppState {
            patterns_dir: tmp.path().join("patterns"),
            workflows_dir: tmp.path().join("workflows"),
            pipelines_dir: tmp.path().join("pipelines"),
            agents_dir,
            index_dir: tmp.path().join("index"),
            config: crate::server::ServerConfig { readonly: false },
            events_tx: tokio::sync::broadcast::channel(64).0,
        };
        for d in [
            &state.patterns_dir,
            &state.workflows_dir,
            &state.pipelines_dir,
        ] {
            std::fs::create_dir_all(d).unwrap();
        }

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = json.as_array().expect("response is an array");
        assert_eq!(arr.len(), 2);

        let alpha = arr.iter().find(|a| a["name"] == "alpha").unwrap();
        assert_eq!(alpha["display_name"], "Alpha Bot");
        assert_eq!(alpha["status"], "running");

        let beta = arr.iter().find(|a| a["name"] == "beta").unwrap();
        assert_eq!(beta["status"], "stopped");
    }
}
