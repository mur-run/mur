//! `GET /api/v1/agents/{name}/telemetry` and `WS /api/v1/agents/{name}/stream`.

use std::io::SeekFrom;
use std::sync::Arc;

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt, BufReader};
use tokio::time::{Duration, sleep};

use crate::server::{AppError, AppState};

#[derive(Deserialize)]
pub struct TelemetryQuery {
    /// `YYYY-MM-DD`. Defaults to today (UTC).
    pub date: Option<String>,
    /// Drop lines whose `ts` field is `<= since` (RFC 3339 string compare —
    /// telemetry timestamps are always UTC ISO-8601 so lex order = time order).
    pub since: Option<String>,
    /// Cap returned lines (keeps the most recent `n`). Default: no cap.
    pub limit: Option<usize>,
}

pub async fn tail_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<TelemetryQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let home = super::agent_home(&state.agents_dir, &name);
    if !home.exists() {
        return Err(AppError::NotFound(format!("agent '{name}' not found")));
    }
    let date = q.date.unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
    let path = home.join("telemetry").join(format!("{date}.jsonl"));

    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(AppError::Internal(
                anyhow::Error::from(e).context(format!("read {}", path.display())),
            ));
        }
    };

    let mut out: Vec<serde_json::Value> = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines
        };
        if let Some(ref since) = q.since
            && let Some(ts) = v.get("ts").and_then(|t| t.as_str())
            && ts.as_bytes() <= since.as_bytes()
        {
            continue;
        }
        out.push(v);
    }
    if let Some(n) = q.limit
        && out.len() > n
    {
        let drop = out.len() - n;
        out.drain(0..drop);
    }

    Ok(Json(out))
}

// ─── WebSocket stream handler ───────────────────────────────────────

const POLL_INTERVAL_MS: u64 = 250;

pub async fn stream_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let home = super::agent_home(&state.agents_dir, &name);
    if !home.exists() {
        return (StatusCode::NOT_FOUND, format!("agent '{name}' not found")).into_response();
    }
    ws.on_upgrade(move |socket| stream_loop(socket, home))
}

async fn stream_loop(mut socket: WebSocket, home: std::path::PathBuf) {
    // Always tail today's file (UTC). On midnight rollover the loop
    // re-resolves the path on the next tick.
    let mut current_date = String::new();
    let mut pos: u64 = 0;
    let mut leftover = String::new();

    loop {
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        if date != current_date {
            current_date = date.clone();
            pos = 0;
            leftover.clear();
            // Start at end-of-file so we only emit *new* lines after connect.
            let path = home.join("telemetry").join(format!("{date}.jsonl"));
            if let Ok(meta) = tokio::fs::metadata(&path).await {
                pos = meta.len();
            }
        }

        let path = home.join("telemetry").join(format!("{current_date}.jsonl"));
        if let Ok(mut f) = tokio::fs::File::open(&path).await {
            let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
            if len < pos {
                // truncated/rotated under us — restart from byte 0
                pos = 0;
                leftover.clear();
            }
            if len > pos && f.seek(SeekFrom::Start(pos)).await.is_ok() {
                let mut buf = String::new();
                if BufReader::new(&mut f).read_to_string(&mut buf).await.is_ok() {
                    pos = len;
                    leftover.push_str(&buf);
                    while let Some(idx) = leftover.find('\n') {
                        let line: String = leftover.drain(..=idx).collect();
                        let line = line.trim_end_matches(['\r', '\n']);
                        if line.is_empty() {
                            continue;
                        }
                        if socket.send(Message::Text(line.to_string().into())).await.is_err() {
                            return; // client disconnected
                        }
                    }
                }
            }
        }

        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
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

    fn write_telemetry(home: &std::path::Path, date: &str, lines: &[&str]) {
        let dir = home.join("telemetry");
        std::fs::create_dir_all(&dir).unwrap();
        let body = lines.join("\n") + "\n";
        std::fs::write(dir.join(format!("{date}.jsonl")), body).unwrap();
    }

    #[tokio::test]
    async fn telemetry_returns_recent_lines_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        write_telemetry(
            &home,
            "2026-04-28",
            &[
                r#"{"ts":"2026-04-28T07:00:00Z","kind":"start"}"#,
                r#"{"ts":"2026-04-28T07:00:01Z","kind":"message_send"}"#,
                r#"{"ts":"2026-04-28T07:00:02Z","kind":"message_done"}"#,
            ],
        );

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/telemetry?date=2026-04-28&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["kind"], "start");
        assert_eq!(arr[2]["kind"], "message_done");
    }

    #[tokio::test]
    async fn telemetry_filters_by_since_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        write_telemetry(
            &home,
            "2026-04-28",
            &[
                r#"{"ts":"2026-04-28T07:00:00Z","kind":"start"}"#,
                r#"{"ts":"2026-04-28T07:00:05Z","kind":"message_send"}"#,
                r#"{"ts":"2026-04-28T07:00:10Z","kind":"message_done"}"#,
            ],
        );

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/telemetry?date=2026-04-28&since=2026-04-28T07:00:04Z")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2); // dropped the 07:00:00 line
        assert_eq!(arr[0]["kind"], "message_send");
    }

    #[tokio::test]
    async fn telemetry_returns_empty_when_no_file_for_date() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/telemetry?date=1999-01-01")
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
    async fn telemetry_returns_404_for_missing_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let app = build_router(build_state(&tmp));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/ghost/telemetry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn ws_stream_emits_lines_appended_after_connect() {
        use tokio_tungstenite::tungstenite;

        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        std::fs::create_dir_all(home.join("telemetry")).unwrap();
        let log_path = home.join("telemetry").join(format!("{date}.jsonl"));
        std::fs::write(&log_path, "").unwrap();

        let app = build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("ws://127.0.0.1:{port}/api/v1/agents/alpha/stream");
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Append after the WS is connected
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::fs::write(
            &log_path,
            format!(
                r#"{{"ts":"{date}T00:00:00Z","kind":"hello"}}
"#
            ),
        )
        .unwrap();

        // Read one frame within a generous timeout (poll interval is 250ms)
        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), {
            use futures_util::StreamExt;
            async { ws.next().await }
        })
        .await
        .expect("ws frame within 3s")
        .expect("stream not closed")
        .expect("frame ok");

        match msg {
            tungstenite::Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                assert_eq!(v["kind"], "hello");
            }
            other => panic!("expected text frame, got {other:?}"),
        }
    }
}
