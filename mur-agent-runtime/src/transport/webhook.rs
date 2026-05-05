//! Track C5 / M5.2 — HTTP webhook receiver (Axum handler + HMAC).
//!
//! This module is the pure protocol layer: parse the incoming
//! `POST /agents/<slug>/webhook`, verify the `X-Mur-Signature`
//! HMAC against a caller-supplied secret, deserialize the body
//! into a [`WebhookPayload`]. Routing the resulting payload through
//! the multimodal pipeline lives in M5.4; the supervisor wiring
//! (start the Axum server, plug in the secret, plug in the
//! ingestor) lives in M5.3.
//!
//! Keeping the handler decoupled from the pipeline + supervisor
//! lets tests drive synthetic Axum requests against the pure
//! verifier and decoder without spinning up the runtime.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Body shape clients POST. Mirrors Track C3's `SharePayload` —
/// channel front-ends already produce this for local channels, so
/// downstream code (M5.4 ingestor bridge) doesn't have to branch on
/// "is this a webhook or a hotkey press".
///
/// `kind` discriminates the value's interpretation:
/// - `text` / `url`: `value` is UTF-8 text
/// - `image` / `file`: `value` is base64 (URL-safe, no padding)
///   bytes; M5.4 writes them to a temp file and routes via
///   `process_artifact`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhookPayload {
    pub kind: WebhookKind,
    pub value: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebhookKind {
    Text,
    Url,
    Image,
    File,
}

/// Successful response — sha256 of the body acts as a lightweight
/// receipt the sender can correlate against retries.
#[derive(Debug, Serialize)]
pub struct WebhookAck {
    pub id: String,
    pub queued_at: String,
}

/// Per-listener state. Holds the HMAC secret, the agent slug we
/// answer for, and the agent home dir the multimodal pipeline writes
/// provenance into.
#[derive(Clone)]
pub struct WebhookState {
    pub agent_slug: String,
    pub hmac_secret: Arc<[u8]>,
    /// Maximum body size in bytes. Hard-coded 10 MiB; the handler
    /// returns 413 before HMAC check so giant signed bodies can't
    /// stall the verifier.
    pub max_body_bytes: usize,
    /// Agent home (`~/.mur/agents/<slug>/`). The pipeline writes
    /// `telemetry/inputs.jsonl` + `inputs/<sha256>.txt` under here.
    pub agent_home: std::path::PathBuf,
    /// M5.5 — per-source token-bucket rate limit. Keyed by remote
    /// IP (or `X-Mur-Source` header when present). Defaults to
    /// 60 requests / 60 s per source.
    pub limiter: Arc<TokenBucketLimiter>,
}

impl WebhookState {
    pub fn new(
        agent_slug: impl Into<String>,
        hmac_secret: impl AsRef<[u8]>,
        agent_home: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            agent_slug: agent_slug.into(),
            hmac_secret: Arc::from(hmac_secret.as_ref().to_vec()),
            max_body_bytes: 10 * 1024 * 1024,
            agent_home: agent_home.into(),
            limiter: Arc::new(TokenBucketLimiter::default()),
        }
    }
}

/// Per-source token-bucket rate limit.
///
/// Each source key gets `capacity` tokens that refill at
/// `refill_per_sec`. A request consumes one token; if the bucket is
/// empty, the handler returns 429.
///
/// Source key precedence: `X-Mur-Source` header (sender-supplied
/// identifier — useful when many CI runs share an egress IP) →
/// `ConnectInfo::<SocketAddr>` peer IP → fallback `"unknown"`.
///
/// Buckets are kept in a process-local `Mutex<HashMap>` — fine for
/// the single-agent listener M5.3 wires; if multi-agent listeners
/// ever share a port, the limiter would need an outer key for the
/// agent slug too.
pub struct TokenBucketLimiter {
    capacity: f64,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucketLimiter {
    /// `capacity` tokens, refilled `capacity / window` per second.
    pub fn new(capacity: u32, window: Duration) -> Self {
        let cap_f = capacity as f64;
        let secs = window.as_secs_f64().max(0.001);
        Self {
            capacity: cap_f,
            refill_per_sec: cap_f / secs,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Try to consume one token for `key`. Returns `true` if the
    /// request is allowed, `false` if the bucket was empty.
    pub fn try_acquire(&self, key: &str) -> bool {
        self.try_acquire_at(key, Instant::now())
    }

    /// Test seam — same logic but with an injected `now` so unit
    /// tests can advance time deterministically.
    pub fn try_acquire_at(&self, key: &str, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        let bucket = buckets.entry(key.to_string()).or_insert(Bucket {
            tokens: self.capacity,
            last_refill: now,
        });
        // Refill: tokens accrue linearly since last check.
        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last_refill = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl Default for TokenBucketLimiter {
    /// 60 requests per minute per source. Plenty for real CI
    /// senders; comfortably below "obvious flood".
    fn default() -> Self {
        Self::new(60, Duration::from_secs(60))
    }
}

/// Build the Axum router. Caller wires the supervisor's `bind:port`
/// (M5.3) and serves it with `axum::serve`.
pub fn router(state: WebhookState) -> Router {
    Router::new()
        .route("/agents/{slug}/webhook", post(handle_webhook))
        .with_state(state)
}

/// Resolve the rate-limit source key for a request.
///
/// Senders supply `X-Mur-Source: <identifier>` to scope buckets to
/// their CI job / pipeline / app. Anonymous requests share the
/// `"default"` bucket — coarse but enough to throttle a misbehaving
/// caller. Per-IP scoping needs Axum's `ConnectInfo` extractor +
/// `into_make_service_with_connect_info`; the listener wires that up
/// already, but extracting in the handler currently runs into a
/// trait-bound issue we'll revisit when we bring a typed
/// `WebhookExtractors` struct online.
fn rate_limit_key(headers: &HeaderMap) -> String {
    headers
        .get("x-mur-source")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "default".to_string())
}

/// `POST /agents/<slug>/webhook` handler.
///
/// Returns:
/// - 202 + `WebhookAck` on success
/// - 401 on signature mismatch / missing header
/// - 404 on slug mismatch (don't leak which agents exist)
/// - 413 if body > `max_body_bytes`
/// - 415 / 422 on malformed JSON / unknown kind
async fn handle_webhook(
    State(state): State<WebhookState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if slug != state.agent_slug {
        return (StatusCode::NOT_FOUND, "agent not found").into_response();
    }
    if body.len() > state.max_body_bytes {
        return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    }
    // Rate limit BEFORE HMAC check so a flood of unsigned junk
    // can't burn CPU on signature verifies.
    let key = rate_limit_key(&headers);
    if !state.limiter.try_acquire(&key) {
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }
    let signature_header = match headers.get("x-mur-signature").and_then(|v| v.to_str().ok()) {
        Some(s) => s,
        None => return (StatusCode::UNAUTHORIZED, "missing X-Mur-Signature").into_response(),
    };
    if !verify_signature(&state.hmac_secret, &body, signature_header) {
        return (StatusCode::UNAUTHORIZED, "signature mismatch").into_response();
    }
    let payload: WebhookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid JSON: {e}"),
            )
                .into_response();
        }
    };
    let id = body_sha256(&body);
    let queued_at = chrono::Utc::now().to_rfc3339();
    // M5.4 — route the validated payload through the multimodal
    // pipeline. Pipeline calls block on disk I/O; offload to the
    // blocking pool so the Axum task doesn't stall.
    let agent_home = state.agent_home.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || dispatch_to_pipeline(&payload, &agent_home))
        .await
        .map_err(|e| anyhow::anyhow!("webhook dispatch task panicked: {e}"))
        .and_then(|inner| inner)
    {
        tracing::warn!(slug = %slug, id = %id, error = %e, "webhook pipeline dispatch failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("pipeline dispatch failed: {e}"),
        )
            .into_response();
    }
    tracing::info!(slug = %slug, id = %id, "webhook ingested");
    (StatusCode::ACCEPTED, Json(WebhookAck { id, queued_at })).into_response()
}

/// Hand a verified [`WebhookPayload`] to the same pipeline entry
/// points the local channels (Track C3) call:
///
/// - `text` / `url` → `process_share_text` (prepends `--- share\n`
///   marker so B0 wraps the body as `<untrusted_share>` on the
///   next prompt-submit).
/// - `image` / `file` → decode base64, write to a temp file, hand
///   bytes to `process_artifact` with a mime sniffed from the
///   metadata or filename hint.
///
/// All work happens synchronously here; the caller wraps with
/// `spawn_blocking` to keep the async runtime responsive.
pub fn dispatch_to_pipeline(
    payload: &WebhookPayload,
    agent_home: &std::path::Path,
) -> anyhow::Result<()> {
    use crate::multimodal::pipeline::{process_artifact, process_share_text};
    use base64::Engine;

    match payload.kind {
        WebhookKind::Text | WebhookKind::Url => {
            process_share_text(&payload.value, "webhook", agent_home)?;
            Ok(())
        }
        WebhookKind::Image | WebhookKind::File => {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload.value.as_bytes())
                .or_else(|_| {
                    base64::engine::general_purpose::STANDARD.decode(payload.value.as_bytes())
                })
                .map_err(|e| anyhow::anyhow!("invalid base64 for {:?} kind: {e}", payload.kind))?;
            // Senders may include a `metadata.filename` hint we can
            // mime-sniff against; falling back to the kind tag's
            // generic mime keeps `process_artifact`'s router happy.
            let filename_hint = payload
                .metadata
                .get("filename")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mime = if !filename_hint.is_empty() {
                mime_guess::from_path(filename_hint)
                    .first_or_octet_stream()
                    .essence_str()
                    .to_string()
            } else {
                match payload.kind {
                    WebhookKind::Image => "image/png".to_string(),
                    _ => "application/octet-stream".to_string(),
                }
            };
            process_artifact(&bytes, &mime, agent_home)?;
            Ok(())
        }
    }
}

/// Constant-time HMAC-SHA256 verify. `header` is in the form
/// `sha256=<hex>`; rejects anything else.
pub fn verify_signature(secret: &[u8], body: &[u8], header: &str) -> bool {
    let Some(hex) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(provided) = hex::decode(hex) else {
        return false;
    };
    let mut mac = match <Hmac<Sha256> as Mac>::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = mac.finalize().into_bytes();
    // ConstantTimeEq for fixed-length slices; mismatched lengths
    // return early via subtle's `ct_eq` shortcut.
    computed.ct_eq(provided.as_slice()).into()
}

fn body_sha256(body: &[u8]) -> String {
    use sha2::Digest;
    let h = Sha256::digest(body);
    hex::encode(h)
}

/// Listener handle returned by [`spawn_webhook_listener`].
///
/// Holds the resolved `local_addr` (helpful when the user binds
/// `0.0.0.0:0` for ephemeral testing) and a `JoinHandle` so the
/// supervisor can `.abort()` the task during graceful shutdown.
pub struct WebhookHandle {
    pub local_addr: std::net::SocketAddr,
    join: tokio::task::JoinHandle<()>,
}

impl WebhookHandle {
    /// Block until the listener task exits. Mirrors the TCP
    /// listener's `await_shutdown` so supervisor wiring can use the
    /// same pattern across transports.
    pub async fn await_shutdown(self) {
        let _ = self.join.await;
    }

    /// Force-stop the listener immediately. Used by the supervisor
    /// on SIGTERM where we don't want to wait for in-flight requests.
    pub fn abort(&self) {
        self.join.abort();
    }
}

/// Bind the Axum router to `bind:port` and start serving.
///
/// Synchronous up to the bind step so the supervisor can surface
/// "address already in use" before the agent claims to be ready;
/// once the listener is bound the actual `axum::serve` future moves
/// onto a tokio task.
pub async fn spawn_webhook_listener(
    bind: &str,
    port: u16,
    state: WebhookState,
) -> anyhow::Result<WebhookHandle> {
    use anyhow::Context;
    let addr = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind webhook listener at {addr}"))?;
    let local_addr = listener.local_addr().context("read local_addr")?;
    let app = router(state);
    // `into_make_service_with_connect_info::<SocketAddr>()` lets
    // the handler extract the peer IP via `ConnectInfo` for the
    // M5.5 rate limiter. Without it, `ConnectInfo` extraction
    // would always 500.
    let make_service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let join = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, make_service).await {
            tracing::warn!(error = %e, "webhook listener exited with error");
        }
    });
    Ok(WebhookHandle { local_addr, join })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn router_for(secret: &[u8]) -> Router {
        // Tests don't exercise the pipeline arm — handler-level
        // assertions stop at 202 / 401 / 404 / 413 / 422; tempdir
        // gives us a valid `agent_home` so the dispatcher's
        // disk-IO path succeeds when reached (covered by
        // `dispatch_to_pipeline` tests below).
        let tmp = tempfile::tempdir().expect("tempdir");
        router(WebhookState::new("coach", secret, tmp.keep()))
    }

    #[test]
    fn verify_signature_accepts_correct_mac() {
        let secret = b"super-secret";
        let body = b"{\"kind\":\"text\",\"value\":\"hi\"}";
        let header = sign(secret, body);
        assert!(verify_signature(secret, body, &header));
    }

    #[test]
    fn verify_signature_rejects_bad_mac() {
        let secret = b"super-secret";
        let body = b"hello";
        // Same length as a valid sha256= hex but different content.
        let header = format!("sha256={}", "0".repeat(64));
        assert!(!verify_signature(secret, body, &header));
    }

    #[test]
    fn verify_signature_rejects_missing_prefix() {
        assert!(!verify_signature(b"x", b"y", "abcdef"));
    }

    #[test]
    fn verify_signature_rejects_invalid_hex() {
        assert!(!verify_signature(b"x", b"y", "sha256=zzz"));
    }

    #[tokio::test]
    async fn handler_accepts_signed_text_payload() {
        let secret = b"super-secret";
        let body = br#"{"kind":"text","value":"hello","metadata":{}}"#;
        let header = sign(secret, body);
        let req = Request::builder()
            .method("POST")
            .uri("/agents/coach/webhook")
            .header("x-mur-signature", &header)
            .header("content-type", "application/json")
            .body(Body::from(&body[..]))
            .unwrap();
        let resp = router_for(secret).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn handler_rejects_unsigned_request() {
        let secret = b"super-secret";
        let body = br#"{"kind":"text","value":"hello"}"#;
        let req = Request::builder()
            .method("POST")
            .uri("/agents/coach/webhook")
            .header("content-type", "application/json")
            .body(Body::from(&body[..]))
            .unwrap();
        let resp = router_for(secret).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn handler_rejects_wrong_signature() {
        let secret = b"super-secret";
        let body = br#"{"kind":"text","value":"hello"}"#;
        let req = Request::builder()
            .method("POST")
            .uri("/agents/coach/webhook")
            .header("x-mur-signature", "sha256=deadbeef")
            .body(Body::from(&body[..]))
            .unwrap();
        let resp = router_for(secret).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn handler_rejects_wrong_slug() {
        let secret = b"super-secret";
        let body = br#"{"kind":"text","value":"hi"}"#;
        let header = sign(secret, body);
        let req = Request::builder()
            .method("POST")
            .uri("/agents/intruder/webhook")
            .header("x-mur-signature", header)
            .body(Body::from(&body[..]))
            .unwrap();
        let resp = router_for(secret).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn handler_rejects_oversize_body() {
        let secret = b"x";
        // 11 MiB of zeros, signed correctly. Should still 413
        // because size check runs before signature check.
        let body = vec![0u8; 11 * 1024 * 1024];
        let header = sign(secret, &body);
        let req = Request::builder()
            .method("POST")
            .uri("/agents/coach/webhook")
            .header("x-mur-signature", header)
            .body(Body::from(body))
            .unwrap();
        let resp = router_for(secret).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn handler_rejects_malformed_json() {
        let secret = b"super-secret";
        let body = b"not json {";
        let header = sign(secret, body);
        let req = Request::builder()
            .method("POST")
            .uri("/agents/coach/webhook")
            .header("x-mur-signature", header)
            .body(Body::from(&body[..]))
            .unwrap();
        let resp = router_for(secret).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn dispatch_text_writes_inputs_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = WebhookPayload {
            kind: WebhookKind::Text,
            value: "hello from CI".into(),
            metadata: serde_json::json!({}),
        };
        dispatch_to_pipeline(&payload, tmp.path()).unwrap();
        let ledger = tmp.path().join("telemetry").join("inputs.jsonl");
        assert!(ledger.exists(), "inputs.jsonl should be written");
        let body = std::fs::read_to_string(&ledger).unwrap();
        assert!(body.contains("share:webhook") || body.contains("\"webhook\""));
    }

    #[test]
    fn dispatch_image_decodes_base64() {
        use base64::Engine;
        let tmp = tempfile::tempdir().unwrap();
        // Tiny PNG (8 bytes of header — pipeline only sniffs mime).
        let png = b"\x89PNG\r\n\x1a\n";
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(png);
        let payload = WebhookPayload {
            kind: WebhookKind::Image,
            value: b64,
            metadata: serde_json::json!({"filename": "screenshot.png"}),
        };
        dispatch_to_pipeline(&payload, tmp.path()).unwrap();
        let ledger = tmp.path().join("telemetry").join("inputs.jsonl");
        assert!(ledger.exists());
    }

    #[test]
    fn token_bucket_consumes_then_blocks() {
        let limiter = TokenBucketLimiter::new(3, Duration::from_secs(60));
        let now = Instant::now();
        // First 3 acquires succeed (full bucket).
        assert!(limiter.try_acquire_at("k", now));
        assert!(limiter.try_acquire_at("k", now));
        assert!(limiter.try_acquire_at("k", now));
        // 4th hits the floor.
        assert!(!limiter.try_acquire_at("k", now));
    }

    #[test]
    fn token_bucket_refills_after_window() {
        let limiter = TokenBucketLimiter::new(2, Duration::from_secs(2));
        let t0 = Instant::now();
        assert!(limiter.try_acquire_at("k", t0));
        assert!(limiter.try_acquire_at("k", t0));
        assert!(!limiter.try_acquire_at("k", t0));
        // 1 s later: refill rate = 2 tokens / 2 s = 1 token/sec, so
        // we get 1 token back.
        let t1 = t0 + Duration::from_secs(1);
        assert!(limiter.try_acquire_at("k", t1));
        assert!(!limiter.try_acquire_at("k", t1));
    }

    #[test]
    fn token_bucket_isolates_keys() {
        let limiter = TokenBucketLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.try_acquire_at("alpha", now));
        // beta starts with its own full bucket.
        assert!(limiter.try_acquire_at("beta", now));
        assert!(!limiter.try_acquire_at("alpha", now));
        assert!(!limiter.try_acquire_at("beta", now));
    }

    #[test]
    fn rate_limit_key_uses_x_mur_source_when_present() {
        use axum::http::HeaderValue;
        let mut h = HeaderMap::new();
        h.insert(
            "x-mur-source",
            HeaderValue::from_static("github-actions/myrepo"),
        );
        assert_eq!(rate_limit_key(&h), "github-actions/myrepo");
    }

    #[test]
    fn rate_limit_key_falls_back_to_default() {
        let h = HeaderMap::new();
        assert_eq!(rate_limit_key(&h), "default");
    }

    #[tokio::test]
    async fn handler_returns_429_when_limiter_exhausted() {
        let secret = b"x";
        // Capacity = 1, large window so the second request can't
        // refill before the assertion.
        let tmp = tempfile::tempdir().unwrap();
        let mut state = WebhookState::new("coach", secret, tmp.keep());
        state.limiter = Arc::new(TokenBucketLimiter::new(1, Duration::from_secs(3600)));
        let app = router(state);

        let body = br#"{"kind":"text","value":"hi"}"#;
        let header = sign(secret, body);
        let make_req = || {
            Request::builder()
                .method("POST")
                .uri("/agents/coach/webhook")
                .header("x-mur-signature", &header)
                .header("x-mur-source", "test-burst")
                .body(Body::from(&body[..]))
                .unwrap()
        };
        let r1 = app.clone().oneshot(make_req()).await.unwrap();
        assert_eq!(r1.status(), StatusCode::ACCEPTED);
        let r2 = app.oneshot(make_req()).await.unwrap();
        assert_eq!(r2.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn dispatch_image_rejects_invalid_base64() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = WebhookPayload {
            kind: WebhookKind::Image,
            value: "this is not base64!!!".into(),
            metadata: serde_json::json!({}),
        };
        let err = dispatch_to_pipeline(&payload, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("invalid base64"));
    }

    #[tokio::test]
    async fn handler_rejects_unknown_kind() {
        let secret = b"super-secret";
        // Anything that isn't text/url/image/file fails serde's
        // tagged-enum match.
        let body = br#"{"kind":"voice","value":"x"}"#;
        let header = sign(secret, body);
        let req = Request::builder()
            .method("POST")
            .uri("/agents/coach/webhook")
            .header("x-mur-signature", header)
            .body(Body::from(&body[..]))
            .unwrap();
        let resp = router_for(secret).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
