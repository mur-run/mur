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

use std::sync::Arc;

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

/// Per-listener state. Holds the HMAC secret + the agent slug we
/// answer for; the slug guards against `POST /agents/<other>/webhook`
/// leaking onto our port.
#[derive(Clone)]
pub struct WebhookState {
    pub agent_slug: String,
    pub hmac_secret: Arc<[u8]>,
    /// Maximum body size in bytes. M5.5 wires this from config; M5.2
    /// hard-codes 10 MiB so the handler returns 413 instead of
    /// blowing up on a malicious sender.
    pub max_body_bytes: usize,
}

impl WebhookState {
    pub fn new(agent_slug: impl Into<String>, hmac_secret: impl AsRef<[u8]>) -> Self {
        Self {
            agent_slug: agent_slug.into(),
            hmac_secret: Arc::from(hmac_secret.as_ref().to_vec()),
            max_body_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Build the Axum router. Caller wires the supervisor's `bind:port`
/// (M5.3) and serves it with `axum::serve`.
pub fn router(state: WebhookState) -> Router {
    Router::new()
        .route("/agents/{slug}/webhook", post(handle_webhook))
        .with_state(state)
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
    // M5.4 will hand `payload` to the multimodal pipeline here.
    // For M5.2 we acknowledge and drop — the unit tests assert the
    // verifier + parser surface; the pipeline path gets covered in
    // its own milestone.
    tracing::info!(slug = %slug, kind = ?payload.kind, id = %id, "webhook received (M5.4 will route)");
    (StatusCode::ACCEPTED, Json(WebhookAck { id, queued_at })).into_response()
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
    let join = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
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
        router(WebhookState::new("coach", secret))
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
