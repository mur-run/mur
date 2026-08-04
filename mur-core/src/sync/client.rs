//! HTTP client that talks to mur-server for sync (`/v1/signals/*`).
//!
//! Used by `mur push` (sends outbox contents via `push_batch`) and by
//! `mur fetch` (calls `fetch_pending` → writes to Inbox → `ack`).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_common::{Pattern, Scope, Signal};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

/// One draft as returned by `GET /api/v1/core/drafts/pending`.
///
/// Mirrors a subset of `models.PatternDraft` (server-side). We use the
/// server's JSON tag names verbatim; fields we don't need on the client
/// (`signal_id`, `source_actor`, `reject_reason`, `reviewed_at`) are
/// intentionally omitted — `serde` ignores unknown keys.
#[derive(Debug, Deserialize, Clone)]
pub struct DraftRecord {
    pub id: Uuid,
    pub scope: Scope,
    pub payload: Pattern,
    #[serde(default)]
    pub origin_context: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// Response body of `GET /api/v1/core/drafts/pending`.
#[derive(Debug, Deserialize, Clone)]
pub struct DraftsPendingResponse {
    pub drafts: Vec<DraftRecord>,
    pub next_cursor: Option<String>,
}

/// One pending skill draft from `GET /api/v1/core/skills/pending`.
#[derive(Debug, Deserialize, Clone)]
pub struct SkillDraft {
    pub id: String,
    /// Raw SkillManifest JSON from the server.
    pub payload: String,
    pub origin_context: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PendingSkillDraftsResponse {
    pub drafts: Vec<SkillDraft>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct RejectDraftRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
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

    /// Fetch a page of pending drafts for the caller. The server paginates via
    /// an opaque `next_cursor` string — passing it back as `cursor` retrieves
    /// the next page. `limit` is passed as the `limit` query param.
    pub async fn fetch_drafts(
        &self,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<DraftsPendingResponse> {
        let mut url = format!(
            "{}/api/v1/core/drafts/pending?limit={}",
            self.base_url, limit
        );
        if let Some(c) = cursor {
            url.push_str("&since=");
            url.push_str(&urlencoding::encode(c));
        }
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| "fetch_drafts non-2xx")?;
        resp.json::<DraftsPendingResponse>()
            .await
            .context("decode DraftsPendingResponse")
    }

    /// Reject a pending draft. Returns `Ok(())` on any 2xx (server returns
    /// 204 No Content on success). Maps 403 → "not owned by you" and 404 →
    /// "not found" with the draft id in the message.
    pub async fn reject_draft(&self, id: Uuid, reason: Option<&str>) -> Result<()> {
        let url = format!("{}/api/v1/core/drafts/{}/reject", self.base_url, id);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&RejectDraftRequest { reason })
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            StatusCode::FORBIDDEN => Err(anyhow::anyhow!("draft {id} not owned by you")),
            StatusCode::NOT_FOUND => Err(anyhow::anyhow!("draft {id} not found")),
            other => Err(anyhow::anyhow!("reject_draft {id} failed: HTTP {other}")),
        }
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

    /// Fetch a page of pending skill drafts proposed by peers.
    pub async fn fetch_pending_skill_drafts(
        &self,
        cursor: Option<&str>,
    ) -> Result<PendingSkillDraftsResponse> {
        let url = match cursor {
            Some(c) => format!("{}/api/v1/core/skills/pending?since={}", self.base_url, c),
            None => format!("{}/api/v1/core/skills/pending", self.base_url),
        };
        self.http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()
            .with_context(|| "fetch_pending_skill_drafts non-2xx")?
            .json()
            .await
            .map_err(anyhow::Error::from)
    }

    /// Acknowledge (accept) a pending skill draft by id.
    pub async fn ack_skill_draft(&self, id: &str) -> Result<()> {
        let url = format!("{}/api/v1/core/skills/{}/ack", self.base_url, id);
        self.http
            .post(&url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()
            .with_context(|| "ack_skill_draft non-2xx")?;
        Ok(())
    }

    /// Reject a pending skill draft by id.
    pub async fn reject_skill_draft(&self, id: &str, reason: &str) -> Result<()> {
        let url = format!("{}/api/v1/core/skills/{}/reject", self.base_url, id);
        self.http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "reason": reason }))
            .send()
            .await?
            .error_for_status()
            .with_context(|| "reject_skill_draft non-2xx")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, SignalKind, SignalTarget};
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
            sig: None,
            key_version: 0,
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
        assert!(
            msg.contains("non-2xx"),
            "expected non-2xx context, got: {msg}"
        );
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

    // ─── Draft API tests ─────────────────────────────────────────

    fn sample_draft_json(id: Uuid, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "signal_id": Uuid::new_v4(),
            "actor_user_id": Uuid::new_v4(),
            "scope": {"kind": "personal"},
            "source_actor": {
                "source": "Slack",
                "native_id": "U1",
                "display_name": null,
                "resolved_user_id": null
            },
            "payload": {
                "schema": 2,
                "name": name,
                "description": "A draft pattern",
                "content": "use pnpm not npm",
                "tier": "project"
            },
            "origin_context": "#eng chat",
            "confidence": 0.87,
            "status": "pending",
            "created_at": "2026-04-22T10:30:00Z",
            "reviewed_at": null
        })
    }

    #[tokio::test]
    async fn fetch_drafts_happy_path_parses_payload() {
        let server = MockServer::start().await;
        let id = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path("/api/v1/core/drafts/pending"))
            .and(query_param("limit", "50"))
            .and(header("authorization", "Bearer TOK"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "drafts": [sample_draft_json(id, "use-pnpm-not-npm")],
                "next_cursor": null
            })))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "TOK").unwrap();
        let r = c.fetch_drafts(None, 50).await.unwrap();
        assert_eq!(r.drafts.len(), 1);
        assert_eq!(r.drafts[0].id, id);
        assert_eq!(r.drafts[0].payload.name, "use-pnpm-not-npm");
        assert_eq!(r.drafts[0].scope, Scope::Personal);
        assert!(r.next_cursor.is_none());
    }

    #[tokio::test]
    async fn fetch_drafts_passes_since_cursor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/drafts/pending"))
            .and(query_param("limit", "10"))
            .and(query_param("since", "cur1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "drafts": [],
                "next_cursor": "cur2"
            })))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "TOK").unwrap();
        let r = c.fetch_drafts(Some("cur1"), 10).await.unwrap();
        assert_eq!(r.next_cursor.as_deref(), Some("cur2"));
    }

    #[tokio::test]
    async fn reject_draft_happy_path_sends_reason() {
        let server = MockServer::start().await;
        let id = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/core/drafts/{id}/reject")))
            .and(header("authorization", "Bearer TOK"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "TOK").unwrap();
        c.reject_draft(id, Some("not useful")).await.unwrap();
    }

    #[tokio::test]
    async fn reject_draft_forbidden_maps_to_owner_error() {
        let server = MockServer::start().await;
        let id = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/core/drafts/{id}/reject")))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "TOK").unwrap();
        let err = c.reject_draft(id, None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not owned by you"), "got: {msg}");
    }

    #[tokio::test]
    async fn reject_draft_not_found_maps_to_not_found_error() {
        let server = MockServer::start().await;
        let id = Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/core/drafts/{id}/reject")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let c = SyncClient::new(server.uri(), "TOK").unwrap();
        let err = c.reject_draft(id, None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not found"), "got: {msg}");
    }

    #[tokio::test]
    async fn fetch_pending_skill_drafts_parses_response() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/skills/pending"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "drafts": [{"id": "d1", "payload": "{\"name\":\"foo\"}", "origin_context": "chat"}],
                "next_cursor": null
            })))
            .mount(&mock)
            .await;

        let c = SyncClient::new(mock.uri(), "tok").unwrap();
        let r = c.fetch_pending_skill_drafts(None).await.unwrap();
        assert_eq!(r.drafts.len(), 1);
        assert_eq!(r.drafts[0].id, "d1");
    }

    #[tokio::test]
    async fn ack_skill_draft_sends_post() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/core/skills/d1/ack"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;

        let c = SyncClient::new(mock.uri(), "tok").unwrap();
        c.ack_skill_draft("d1").await.unwrap();
    }
}
