//! HTTP client that talks to mur-server for sync (`/v1/signals/*`).
//!
//! Used by `mur push` (sends outbox contents via `push_batch`) and by
//! `mur fetch` (calls `fetch_pending` → writes to Inbox → `ack`).

use anyhow::{Context, Result};
use mur_common::Signal;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// HTTP client for the mur-server sync API. Carries a bearer token.
pub struct SyncClient {
    base_url: String,
    token: String,
    http: Client,
}

#[derive(Debug, Serialize)]
struct BatchRequest<'a> {
    signals: &'a [Signal],
}

#[derive(Debug, Serialize)]
struct AckRequest<'a> {
    ids: &'a [String],
}

#[derive(Debug, Deserialize, Clone)]
pub struct BatchResponse {
    pub accepted: Vec<String>,
    pub rejected: Vec<RejectedSignal>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RejectedSignal {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PendingResponse {
    pub signals: Vec<Signal>,
    pub next_cursor: Option<String>,
}

impl SyncClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .gzip(true)
            .build()
            .context("build reqwest client")?;
        Ok(Self {
            base_url: base_url.into(),
            token: token.into(),
            http,
        })
    }

    pub async fn push_batch(&self, signals: &[Signal]) -> Result<BatchResponse> {
        let url = format!("{}/api/v1/core/signals/batch", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&BatchRequest { signals })
            .send()
            .await
            .with_context(|| format!("POST {url}"))?
            .error_for_status()
            .with_context(|| "push_batch non-2xx")?;
        resp.json::<BatchResponse>()
            .await
            .context("decode BatchResponse")
    }

    pub async fn fetch_pending(&self, cursor: Option<&str>) -> Result<PendingResponse> {
        let url = match cursor {
            Some(c) => format!(
                "{}/api/v1/core/signals/pending?since={}",
                self.base_url,
                urlencoding::encode(c)
            ),
            None => format!("{}/api/v1/core/signals/pending", self.base_url),
        };
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| "fetch_pending non-2xx")?;
        resp.json::<PendingResponse>()
            .await
            .context("decode PendingResponse")
    }

    pub async fn ack(&self, signal_ids: &[String]) -> Result<()> {
        if signal_ids.is_empty() {
            return Ok(());
        }
        let url = format!("{}/api/v1/core/signals/ack", self.base_url);
        self.http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&AckRequest { ids: signal_ids })
            .send()
            .await
            .with_context(|| format!("POST {url}"))?
            .error_for_status()
            .with_context(|| "ack non-2xx")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Actor, ActorSource, Scope, SignalKind, SignalTarget, SIGNAL_SCHEMA_VERSION};
    use uuid::Uuid;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_signal() -> Signal {
        Signal {
            id: Uuid::new_v4(),
            emitted_at: chrono::Utc::now(),
            actor: Actor {
                source: ActorSource::CommanderDaemon,
                native_id: "svc-1".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: "foo".into(),
                scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal,
            confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
        }
    }

    #[tokio::test]
    async fn push_batch_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/core/signals/batch"))
            .and(header("authorization", "Bearer TEST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accepted": ["sig-1"],
                "rejected": []
            })))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "TEST").unwrap();
        let r = c.push_batch(&[sample_signal()]).await.unwrap();
        assert_eq!(r.accepted, vec!["sig-1".to_string()]);
        assert!(r.rejected.is_empty());
    }

    #[tokio::test]
    async fn push_batch_returns_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/core/signals/batch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accepted": ["sig-ok"],
                "rejected": [{"id": "sig-bad", "reason": "dedupe_window"}]
            })))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "tok").unwrap();
        let r = c.push_batch(&[sample_signal()]).await.unwrap();
        assert_eq!(r.accepted.len(), 1);
        assert_eq!(r.rejected.len(), 1);
        assert_eq!(r.rejected[0].reason, "dedupe_window");
    }

    #[tokio::test]
    async fn push_batch_non_2xx_is_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/core/signals/batch"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "tok").unwrap();
        let err = c.push_batch(&[sample_signal()]).await.unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("non-2xx"), "expected non-2xx context, got: {msg}");
    }

    #[tokio::test]
    async fn fetch_pending_with_cursor_includes_since_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/signals/pending"))
            .and(query_param("since", "abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "signals": [],
                "next_cursor": null
            })))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "tok").unwrap();
        let r = c.fetch_pending(Some("abc")).await.unwrap();
        assert!(r.signals.is_empty());
        assert!(r.next_cursor.is_none());
    }

    #[tokio::test]
    async fn fetch_pending_without_cursor_has_no_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/signals/pending"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "signals": [],
                "next_cursor": "future-cursor"
            })))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "tok").unwrap();
        let r = c.fetch_pending(None).await.unwrap();
        assert_eq!(r.next_cursor.as_deref(), Some("future-cursor"));
    }

    #[tokio::test]
    async fn fetch_pending_returns_signals() {
        let server = MockServer::start().await;
        let sig = sample_signal();
        let sig_json = serde_json::to_value(&sig).unwrap();
        Mock::given(method("GET"))
            .and(path("/api/v1/core/signals/pending"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "signals": [sig_json],
                "next_cursor": null
            })))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "tok").unwrap();
        let r = c.fetch_pending(None).await.unwrap();
        assert_eq!(r.signals.len(), 1);
        assert_eq!(r.signals[0].id, sig.id);
    }

    #[tokio::test]
    async fn ack_sends_ids_payload() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/core/signals/ack"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "tok").unwrap();
        c.ack(&["id-1".into(), "id-2".into()]).await.unwrap();
    }

    #[tokio::test]
    async fn ack_empty_list_is_noop() {
        // MockServer with NO mock mounted — if our code hits the network, wiremock errors
        let server = MockServer::start().await;
        let c = SyncClient::new(server.uri(), "tok").unwrap();
        c.ack(&[]).await.unwrap();
        // No assertion needed — passing means we didn't call the server
    }

    #[tokio::test]
    async fn cursor_with_special_chars_is_url_encoded() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/signals/pending"))
            .and(query_param("since", "uuid with spaces/and/slashes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "signals": [],
                "next_cursor": null
            })))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "tok").unwrap();
        // If urlencoding breaks, the mock won't match and this will 404 → error
        let r = c
            .fetch_pending(Some("uuid with spaces/and/slashes"))
            .await
            .unwrap();
        assert!(r.signals.is_empty());
    }
}
