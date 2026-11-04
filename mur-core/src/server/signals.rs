//! POST /api/v1/core/signals/batch — receive sync signals from commander
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use mur_common::Signal;
use serde::{Deserialize, Serialize};

use super::{AppError, AppState};
use crate::sync::inbox::Inbox;

#[derive(Deserialize)]
pub struct BatchRequest {
    pub signals: Vec<Signal>,
}

#[derive(Serialize)]
pub struct BatchResponse {
    pub accepted: Vec<String>,
    pub rejected: Vec<RejectedSignal>,
}

#[derive(Serialize)]
pub struct RejectedSignal {
    pub id: String,
    pub reason: String,
}

/// Receive a batch of sync signals from commander and apply them to skill stats.
///
/// # Concurrency note
/// This endpoint uses a two-phase write: `receive` appends YAML files to the inbox,
/// then `apply_all` reads and applies them. Skill stats are merged under an
/// exclusive file lock (`SkillStats::merge_in_place`). This is acceptable because
/// the commander daemon runs as a single process with one `FlushService`, so concurrent
/// batches from the same process are not expected in practice.
pub async fn batch_signals(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BatchRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }

    let inbox_dir = state
        .patterns_dir
        .parent()
        .unwrap_or(&state.patterns_dir)
        .join("inbox");
    let inbox = Inbox::new(&inbox_dir).map_err(AppError::Internal)?;

    let mut accepted = Vec::new();
    let mut rejected = Vec::new();

    for signal in &body.signals {
        match inbox.receive(signal) {
            Ok(_) => accepted.push(signal.id.to_string()),
            Err(e) => rejected.push(RejectedSignal {
                id: signal.id.to_string(),
                reason: e.to_string(),
            }),
        }
    }

    if !accepted.is_empty()
        && let Err(e) = inbox.apply_all().map(|report| {
            if !report.errors.is_empty() {
                tracing::warn!(
                    "signals/batch: {} signal(s) accepted but {} failed to apply: {:?}",
                    accepted.len(),
                    report.errors.len(),
                    report.errors
                );
            }
        })
    {
        tracing::error!("signals/batch: apply_all failed: {:#}", e);
    }

    Ok(Json(BatchResponse { accepted, rejected }))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mur_common::{
        Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, Signal, SignalKind, SignalTarget,
    };
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::server::build_router;
    use crate::server::tests::test_state_for_signals;

    /// Stub an installed skill so the inbox applies signals to it.
    fn install_stub_skill(mur_home: &std::path::Path, name: &str) {
        let skill_dir = mur_home.join("skills").join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("skill.yaml"),
            format!("name: {name}\nversion: 1.0.0\n"),
        )
        .unwrap();
    }

    fn exec_success_signal(skill_name: &str) -> Signal {
        Signal {
            id: Uuid::new_v4(),
            emitted_at: chrono::Utc::now(),
            actor: Actor {
                source: ActorSource::Slack,
                native_id: "bot".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Skill {
                name: skill_name.into(),
                scope: Scope::Personal,
            },
            kind: SignalKind::SkillExecutionSuccess,
            scope: Scope::Personal,
            confidence: 0.9,
            schema_version: SIGNAL_SCHEMA_VERSION,
            sig: None,
            key_version: 0,
        }
    }

    #[tokio::test]
    async fn batch_signals_accepted_and_updates_skill_stats() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state_for_signals(&tmp);
        install_stub_skill(tmp.path(), "p1");

        let app = build_router(state);
        let body = serde_json::json!({
            "signals": [exec_success_signal("p1")]
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/core/signals/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["accepted"].as_array().unwrap().len(), 1);
        assert_eq!(json["rejected"].as_array().unwrap().len(), 0);

        let stats = mur_common::skill::stats::SkillStats::load(
            &mur_common::skill::stats::SkillStats::path(tmp.path(), "p1"),
        )
        .unwrap()
        .expect("stats.json written by the inbox");
        assert_eq!(stats.success_count, 1);
    }

    #[tokio::test]
    async fn batch_signals_readonly_returns_403() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = test_state_for_signals(&tmp);
        state.config.readonly = true;

        let app = build_router(state);
        let body = serde_json::json!({ "signals": [] });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/core/signals/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
