//! Node-side commander governance receiver: POST a signed directive, GET the audit.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use mur_common::channel::{ChannelActor, EventKind};
use mur_common::commander::COMMANDER_DIRECTIVE_KEY;

use super::{AppError, AppState};

#[derive(Deserialize)]
pub struct DirectiveEnvelope {
    event: serde_json::Value,
}

/// Accept a commander-signed governance directive and persist it verbatim into the
/// fleet channel so `fold_governance` enforces it. Two-layer auth: the network bearer
/// (require_auth middleware) + the commander Ed25519 signature verified here.
pub async fn post_directive(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DirectiveEnvelope>,
) -> Result<impl IntoResponse, AppError> {
    let event = &body.event;
    let payload = event
        .get("payload")
        .ok_or_else(|| AppError::BadRequest("event.payload missing".into()))?
        .clone();
    let fleet = payload
        .get(COMMANDER_DIRECTIVE_KEY)
        .and_then(|d| d.get("fleet"))
        .and_then(|f| f.as_str())
        .ok_or_else(|| AppError::BadRequest("commander_directive.fleet missing".into()))?
        .to_string();
    let channel_id = format!("fleet-{fleet}");
    let mur_home = state.mur_home();

    let actor: ChannelActor = serde_json::from_value(
        event
            .get("actor")
            .cloned()
            .ok_or_else(|| AppError::BadRequest("event.actor missing".into()))?,
    )
    .map_err(|e| AppError::BadRequest(format!("bad actor: {e}")))?;
    let kind: EventKind = serde_json::from_value(
        event
            .get("kind")
            .cloned()
            .ok_or_else(|| AppError::BadRequest("event.kind missing".into()))?,
    )
    .map_err(|e| AppError::BadRequest(format!("bad kind: {e}")))?;
    let idem = event
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let sig = event
        .get("sig")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Forbidden("event.sig missing".into()))?;
    let key_version = event
        .get("key_version")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);

    // Commander signature (defense-in-depth; fold re-verifies on read).
    let keys = crate::cmd::commander::accepted_pubkeys(&mur_home);
    if keys.is_empty() {
        return Err(AppError::Forbidden("no commander key pinned".into()));
    }
    let sig_ok = keys.iter().any(|pk| {
        mur_channel::sign::verify_event_sig(
            &channel_id,
            &actor,
            kind,
            &payload,
            idem.as_deref(),
            &sig,
            pk,
        )
    });
    if !sig_ok {
        return Err(AppError::Forbidden("invalid commander signature".into()));
    }

    // Channel must already exist — append_event auto-creates the dir, so guard here.
    let store = mur_channel::ChannelStore::new(&mur_home);
    if store.load_manifest(&channel_id).is_err() {
        return Err(AppError::NotFound(format!(
            "unknown fleet channel {channel_id}"
        )));
    }

    // Was this nonce already persisted? (don't double-write the receipt audit.)
    // ponytail: this load_events → append_event → audit sequence makes the receipt
    // row exactly-once for SEQUENTIAL retries (what the single-node engine does) but
    // at-least-once under two truly-concurrent identical POSTs. A duplicate row is
    // benign — the engine's ComplianceChecker matches by nonce and the content_sha256
    // is identical. Tighten with a per-channel lock only if multiple issuers ever race.
    let already = idem.as_deref().is_some_and(|k| {
        store
            .load_events(&channel_id)
            .map(|evs| evs.iter().any(|e| e.idempotency_key.as_deref() == Some(k)))
            .unwrap_or(false)
    });

    let ev = store
        .append_event(
            &channel_id,
            actor.clone(),
            kind,
            payload.clone(),
            idem.clone(),
            Some(sig),
            key_version,
        )
        .map_err(AppError::Internal)?;

    if !already {
        let content =
            mur_channel::sign::sign_input(&channel_id, &actor, kind, &payload, idem.as_deref());
        let content_sha256 = hex::encode(Sha256::digest(&content));
        let directive = payload
            .get(COMMANDER_DIRECTIVE_KEY)
            .and_then(|d| d.get("kind"))
            .and_then(|k| k.as_str())
            .unwrap_or("")
            .to_string();
        if let Ok(audit) = crate::conversations::audit::Audit::open(mur_home.to_str()) {
            let _ = audit.append(
                crate::conversations::audit::AuditAction::Governance {
                    fleet,
                    directive,
                    decision: "received".into(),
                    nonce: idem.unwrap_or_default(),
                },
                content_sha256,
            );
        }
    }

    Ok(Json(json!({ "accepted": true, "seq": ev.seq })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mur_common::channel::{ChannelActor, EventKind};
    use mur_common::commander::COMMANDER_DIRECTIVE_KEY;
    use mur_common::identity::AgentIdentity;
    use std::sync::Arc;
    use tower::ServiceExt;

    // Build AppState (reuses the sibling harness), pin a commander key, create the
    // fleet-dev channel. Returns (state, the pinned identity).
    fn setup(tmp: &tempfile::TempDir) -> (Arc<AppState>, AgentIdentity) {
        let state = crate::server::tests::test_state_for_signals(tmp);
        let home = tmp.path();
        let id = AgentIdentity::generate();
        crate::cmd::commander::cmd_commander_pin(home, &id.public_key_multibase(), false).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("dev", "mur", &[])
            .unwrap();
        (Arc::new(state), id)
    }

    // Build the JSON body the engine POSTs: { "event": <signed ChannelEvent> }.
    fn directive_body(
        id: &AgentIdentity,
        fleet: &str,
        kind: &str,
        budget: Option<f64>,
    ) -> serde_json::Value {
        let nonce = "11111111-1111-1111-1111-111111111111".to_string();
        let channel_id = format!("fleet-{fleet}");
        let payload = serde_json::json!({ COMMANDER_DIRECTIVE_KEY: {
            "kind": kind, "fleet": fleet, "budget_usd": budget,
            "nonce": nonce, "issued_at_ms": 1_000_000u64,
        }});
        let actor = ChannelActor::System;
        let sig = mur_channel::sign::sign_event(
            id,
            &channel_id,
            &actor,
            EventKind::Note,
            &payload,
            Some(&nonce),
        );
        serde_json::json!({ "event": {
            "seq": 0,
            "ts": chrono::Utc::now().to_rfc3339(),
            "actor": serde_json::to_value(&actor).unwrap(),
            "kind": serde_json::to_value(EventKind::Note).unwrap(),
            "payload": payload,
            "idempotency_key": nonce,
            "sig": sig,
            "key_version": serde_json::Value::Null,
        }})
    }

    async fn post(
        state: Arc<AppState>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let app = crate::server::build_router((*state).clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/governance/directive")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn valid_kill_is_persisted_audited_and_folds_killed() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, id) = setup(&tmp);
        let (status, json) = post(state.clone(), directive_body(&id, "dev", "kill", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["accepted"], serde_json::json!(true));

        let home = tmp.path();
        let events = mur_channel::ChannelService::open(home)
            .unwrap()
            .load_events("fleet-dev")
            .unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.payload.get(COMMANDER_DIRECTIVE_KEY).is_some())
        );

        // fold sees the kill
        let keys = [id.verifying_key_bytes()];
        let gov = mur_channel::governance::fold_governance(&events, "fleet-dev", "dev", &keys);
        assert!(gov.killed);

        // received audit row bound to nonce + content_sha256
        let rows = crate::conversations::audit::read_entries(home.to_str()).unwrap();
        let row = rows
            .iter()
            .find(|e| {
                matches!(&e.action,
                    crate::conversations::audit::AuditAction::Governance { decision, .. }
                    if decision == "received")
            })
            .expect("received row");
        assert!(!row.content_sha256.is_empty());
    }

    #[tokio::test]
    async fn wrong_key_is_rejected_403_and_persists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _pinned) = setup(&tmp);
        let attacker = AgentIdentity::generate(); // not the pinned key
        let (status, _) = post(
            state.clone(),
            directive_body(&attacker, "dev", "kill", None),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let events = mur_channel::ChannelService::open(tmp.path())
            .unwrap()
            .load_events("fleet-dev")
            .unwrap();
        assert!(
            events
                .iter()
                .all(|e| e.payload.get(COMMANDER_DIRECTIVE_KEY).is_none())
        );
    }

    #[tokio::test]
    async fn no_commander_pinned_is_403() {
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(crate::server::tests::test_state_for_signals(&tmp));
        mur_channel::ChannelService::open(tmp.path())
            .unwrap()
            .create_for_fleet("dev", "mur", &[])
            .unwrap();
        let id = AgentIdentity::generate(); // signed but no key pinned
        let (status, _) = post(state, directive_body(&id, "dev", "kill", None)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unknown_fleet_is_404() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, id) = setup(&tmp); // only fleet-dev exists
        let (status, _) = post(state, directive_body(&id, "ghost", "kill", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn duplicate_post_is_idempotent_single_audit_row() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, id) = setup(&tmp);
        let body = directive_body(&id, "dev", "kill", None);
        let (s1, j1) = post(state.clone(), body.clone()).await;
        let (s2, j2) = post(state.clone(), body).await;
        assert_eq!(s1, StatusCode::OK);
        assert_eq!(s2, StatusCode::OK);
        assert_eq!(j1["seq"], j2["seq"]); // same event, same seq
        let rows = crate::conversations::audit::read_entries(tmp.path().to_str()).unwrap();
        let received = rows
            .iter()
            .filter(|e| {
                matches!(&e.action,
                    crate::conversations::audit::AuditAction::Governance { decision, .. }
                    if decision == "received")
            })
            .count();
        assert_eq!(received, 1); // not double-written
    }
}
