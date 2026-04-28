//! `GET /api/v1/agents/{name}/evals[/{run_id}]`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use crate::server::{AppError, AppState};

#[derive(Serialize)]
pub struct EvalRunSummary {
    pub run_id: String,
    pub suite: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub summary: serde_json::Value,
}

pub async fn list_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Vec<EvalRunSummary>>, AppError> {
    super::validate_agent_name(&name)?;
    let home = super::agent_home(&state.agents_dir, &name);
    if !home.exists() {
        return Err(AppError::NotFound(format!("agent '{name}' not found")));
    }
    let dir = home.join("evals");
    let entries = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Json(Vec::new()));
        }
        Err(e) => {
            return Err(AppError::Internal(
                anyhow::Error::from(e).context(format!("read_dir {}", dir.display())),
            ));
        }
    };

    let mut out: Vec<EvalRunSummary> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let v: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let run_id = v
            .get("run_id")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if run_id.is_empty() {
            continue;
        }
        let suite = v
            .get("suite")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let started_at = v
            .get("started_at")
            .and_then(|s| s.as_str())
            .map(String::from);
        let finished_at = v
            .get("finished_at")
            .and_then(|s| s.as_str())
            .map(String::from);
        let summary = v.get("summary").cloned().unwrap_or(serde_json::json!({}));
        out.push(EvalRunSummary {
            run_id,
            suite,
            started_at,
            finished_at,
            summary,
        });
    }
    // run_id format is `eval-<ISO8601-no-separators>-<HASH>` (e.g.
    // `eval-20260428T072501-bbbb`). Because the timestamp prefix uses a
    // fixed-width sortable encoding, lexicographic order matches chronological
    // order. Reverse cmp = newest first. Files with malformed run_id are
    // already skipped above.
    out.sort_by(|a, b| b.run_id.cmp(&a.run_id));

    Ok(Json(out))
}

pub async fn detail_handler(
    State(state): State<Arc<AppState>>,
    Path((name, run_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    super::validate_agent_name(&name)?;
    // run_id is used as a filename — reject anything that could escape the
    // evals/ directory (path separators, '..', null bytes, etc.).
    let safe = !run_id.is_empty()
        && run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !safe || run_id.contains("..") {
        return Err(AppError::BadRequest("invalid run_id".to_string()));
    }

    let home = super::agent_home(&state.agents_dir, &name);
    let path = home.join("evals").join(format!("{run_id}.json"));
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::NotFound(format!("eval '{run_id}' not found")));
        }
        Err(e) => {
            return Err(AppError::Internal(
                anyhow::Error::from(e).context(format!("read {}", path.display())),
            ));
        }
    };
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        AppError::Internal(
            anyhow::Error::from(e).context(format!("parse eval json {}", path.display())),
        )
    })?;

    Ok(Json(v))
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::server::{AppState, ServerConfig, build_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    pub(crate) fn build_state(tmp: &tempfile::TempDir) -> AppState {
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

    pub(crate) fn write_eval(home: &std::path::Path, run_id: &str, suite: &str, started_at: &str) {
        let dir = home.join("evals");
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "run_id": run_id,
            "suite": suite,
            "agent": "alpha",
            "started_at": started_at,
            "finished_at": started_at,
            "matrix": {"providers": [], "system_prompts": []},
            "results": [],
            "summary": {"total": 0, "passed": 0, "failed": 0}
        });
        std::fs::write(
            dir.join(format!("{run_id}.json")),
            serde_json::to_vec_pretty(&body).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn list_evals_returns_run_summaries_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        write_eval(
            &home,
            "eval-20260427T100000-aaaa",
            "v1",
            "2026-04-27T10:00:00Z",
        );
        write_eval(
            &home,
            "eval-20260428T072501-bbbb",
            "v1",
            "2026-04-28T07:25:01Z",
        );

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/evals")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["run_id"], "eval-20260428T072501-bbbb"); // newest first
        assert_eq!(arr[1]["run_id"], "eval-20260427T100000-aaaa");
        assert_eq!(arr[0]["suite"], "v1");
    }

    #[tokio::test]
    async fn list_evals_returns_empty_when_no_evals_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/evals")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json, serde_json::json!([]));
    }

    #[tokio::test]
    async fn detail_eval_returns_full_run_json() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        write_eval(
            &home,
            "eval-20260428T072501-bbbb",
            "v1",
            "2026-04-28T07:25:01Z",
        );

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/evals/eval-20260428T072501-bbbb")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["run_id"], "eval-20260428T072501-bbbb");
        assert_eq!(json["suite"], "v1");
        assert!(json.get("results").is_some());
        assert!(json.get("summary").is_some());
    }

    #[tokio::test]
    async fn detail_eval_returns_404_for_unknown_run() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/evals/ghost-run")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn detail_eval_rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/evals/..%2F..%2Fetc%2Fpasswd")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
