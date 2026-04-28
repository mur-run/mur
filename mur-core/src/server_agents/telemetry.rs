//! `GET /api/v1/agents/{name}/telemetry` and `WS /api/v1/agents/{name}/stream`.

use std::collections::VecDeque;
use std::io::BufRead;
use std::io::SeekFrom;
use std::sync::Arc;

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::time::{Duration, sleep};

use crate::server::{AppError, AppState};

fn is_valid_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
}

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
    super::validate_agent_name(&name)?;
    let home = super::agent_home(&state.agents_dir, &name);
    if !home.exists() {
        return Err(AppError::NotFound(format!("agent '{name}' not found")));
    }
    let date = q
        .date
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
    if !is_valid_date(&date) {
        return Err(AppError::BadRequest(format!("invalid date '{date}'")));
    }
    let path = home.join("telemetry").join(format!("{date}.jsonl"));

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Json(Vec::new()));
        }
        Err(e) => {
            return Err(AppError::Internal(
                anyhow::Error::from(e).context(format!("open {}", path.display())),
            ));
        }
    };

    let reader = std::io::BufReader::new(file);
    let mut buffer: VecDeque<serde_json::Value> = VecDeque::new();
    let cap = q.limit;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue, // I/O hiccup mid-file: skip the bad line, keep going
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(ref since) = q.since
            && let Some(ts) = v.get("ts").and_then(|t| t.as_str())
            && ts.as_bytes() <= since.as_bytes()
        {
            continue;
        }
        buffer.push_back(v);
        if let Some(n) = cap
            && buffer.len() > n
        {
            buffer.pop_front();
        }
    }

    Ok(Json(buffer.into_iter().collect()))
}

// ─── WebSocket stream handler ───────────────────────────────────────

const POLL_INTERVAL_MS: u64 = 250;
const MAX_TICK_BYTES: usize = 1024 * 1024; // 1 MiB per tick
const MAX_LEFTOVER_BYTES: usize = 2 * 1024 * 1024; // 2 MiB single-line cap

pub async fn stream_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if super::validate_agent_name(&name).is_err() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            format!("invalid agent name '{name}'"),
        )
            .into_response();
    }
    let home = super::agent_home(&state.agents_dir, &name);
    if !home.exists() {
        return (StatusCode::NOT_FOUND, format!("agent '{name}' not found")).into_response();
    }
    ws.on_upgrade(move |socket| stream_loop(socket, home))
}

async fn stream_loop(mut socket: WebSocket, home: std::path::PathBuf) {
    // Always tail today's file (UTC). On midnight rollover the loop
    // re-resolves the path on the next tick.
    //
    // Assumes append-only writers: detects truncation (`len < pos`) but not
    // file replacement (inode change). External rotation tools that rm+recreate
    // the file may miss lines from the new file.
    let mut current_date = String::new();
    let mut pos: u64 = 0;
    let mut leftover: Vec<u8> = Vec::new();

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
            if len > pos {
                let want = ((len - pos) as usize).min(MAX_TICK_BYTES);
                if f.seek(SeekFrom::Start(pos)).await.is_ok() {
                    let mut buf = vec![0u8; want];
                    if f.read_exact(&mut buf).await.is_ok() {
                        pos += want as u64;
                        leftover.extend_from_slice(&buf);
                        while let Some(idx) = leftover.iter().position(|&b| b == b'\n') {
                            let line_bytes: Vec<u8> = leftover.drain(..=idx).collect();
                            // Trim CR/LF from the trailing edge
                            let trimmed = line_bytes.strip_suffix(b"\n").unwrap_or(&line_bytes);
                            let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
                            if trimmed.is_empty() {
                                continue;
                            }
                            // Skip non-UTF-8 lines silently — telemetry should be ASCII/UTF-8.
                            let s = match std::str::from_utf8(trimmed) {
                                Ok(s) => s,
                                Err(_) => continue,
                            };
                            if socket
                                .send(Message::Text(s.to_string().into()))
                                .await
                                .is_err()
                            {
                                return; // client disconnected
                            }
                        }
                        // Cap unconsumed leftover — single line larger than 2 MiB is corrupt.
                        if leftover.len() > MAX_LEFTOVER_BYTES {
                            leftover.clear();
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
                    .uri(
                        "/api/v1/agents/alpha/telemetry?date=2026-04-28&since=2026-04-28T07:00:04Z",
                    )
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
    async fn telemetry_rejects_invalid_date_format() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/telemetry?date=../../etc/shadow")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
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

    #[tokio::test]
    async fn ws_stream_handles_multi_line_chunk() {
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
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let url = format!("ws://127.0.0.1:{port}/api/v1/agents/alpha/stream");
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Append 3 lines in a single write
        std::fs::write(
            &log_path,
            format!(
                "{{\"ts\":\"{date}T00:00:00Z\",\"kind\":\"a\"}}\n\
                 {{\"ts\":\"{date}T00:00:01Z\",\"kind\":\"b\"}}\n\
                 {{\"ts\":\"{date}T00:00:02Z\",\"kind\":\"c\"}}\n"
            ),
        )
        .unwrap();

        // Collect 3 frames within a generous timeout
        use futures_util::StreamExt;
        let mut kinds: Vec<String> = Vec::new();
        for _ in 0..3 {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(3), ws.next())
                .await
                .expect("frame within 3s")
                .expect("stream not closed")
                .expect("frame ok");
            if let tungstenite::Message::Text(t) = msg {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                kinds.push(v["kind"].as_str().unwrap().to_string());
            }
        }
        assert_eq!(kinds, vec!["a", "b", "c"]);
    }
}
