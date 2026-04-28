//! `GET /api/v1/agents/{name}` — full profile + status.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use crate::server::{AppError, AppState};

#[derive(Serialize)]
pub struct AgentDetail {
    pub profile: mur_common::AgentProfile,
    pub status: AgentStatus,
}

#[derive(Serialize)]
pub struct AgentStatus {
    pub running: bool,
    pub pid: Option<u32>,
}

pub async fn handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<AgentDetail>, AppError> {
    let home = super::agent_home(&state.agents_dir, &name);
    let profile_path = home.join("profile.yaml");

    let yaml = match std::fs::read_to_string(&profile_path) {
        Ok(y) => y,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::NotFound(format!("agent '{name}' not found")));
        }
        Err(e) => {
            return Err(AppError::Internal(
                anyhow::Error::from(e)
                    .context(format!("read {}", profile_path.display())),
            ));
        }
    };

    let profile: mur_common::AgentProfile = serde_yaml_ng::from_str(&yaml)
        .map_err(|e| AppError::Internal(
            anyhow::Error::from(e)
                .context(format!("parse {}", profile_path.display())),
        ))?;

    let pid = std::fs::read_to_string(home.join("running.lock"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let running = super::is_running(&home);

    Ok(Json(AgentDetail {
        profile,
        status: AgentStatus { running, pid },
    }))
}

#[cfg(test)]
mod tests {
    use crate::server::{AppState, ServerConfig, build_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

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

    #[tokio::test]
    async fn detail_returns_full_profile_and_status() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha Bot");
        std::fs::write(home.join("running.lock"), "9999").unwrap();

        let app = build_router(state);
        let resp = app
            .oneshot(Request::builder().uri("/api/v1/agents/alpha").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["profile"]["name"], "alpha");
        assert_eq!(json["profile"]["display_name"], "Alpha Bot");
        assert_eq!(json["profile"]["model"]["provider"], "ollama");
        assert_eq!(json["status"]["running"], true);
        assert_eq!(json["status"]["pid"], 9999);
    }

    #[tokio::test]
    async fn detail_returns_404_for_unknown_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let app = build_router(build_state(&tmp));
        let resp = app
            .oneshot(Request::builder().uri("/api/v1/agents/ghost").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
