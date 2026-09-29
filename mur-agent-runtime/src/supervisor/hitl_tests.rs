use super::*;
use crate::hitl::HitlDecision;
use crate::protocol::a2a_server::MethodHandler;
use serde_json::json;
use std::time::Duration;
use tokio::sync::oneshot;

const TEST_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn test_authority() -> crate::hitl::authority::ApprovalAuthority {
    crate::hitl::authority::ApprovalAuthority::new(Some(TEST_TOKEN.into()), true)
}

#[tokio::test]
async fn hitl_respond_resolves_pending() {
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<HitlDecision>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = oneshot::channel::<HitlDecision>();
    pending.lock().await.insert("test-id".to_string(), tx);

    let handler = HitlRespondHandler {
        pending_approvals: pending.clone(),
        authority: test_authority(),
        shim_trust: Default::default(),
    };
    let result = handler
        .handle(
            Some(json!({"hitl_id": "test-id", "allow": true, "reason": "looks good", "approval_token": TEST_TOKEN})),
            &crate::protocol::a2a_server::RequestContext::none(),
        )
        .await;
    assert!(result.is_ok());

    let decision = rx.await.expect("sender dropped");
    assert!(decision.allow);
    assert_eq!(decision.reason.as_deref(), Some("looks good"));
}

#[tokio::test]
async fn hitl_respond_after_timeout_reports_approval_expired() {
    // A decision arriving after the gate timed out (entry removed) must
    // surface as a clear "approval expired" error with its own code —
    // NOT the cryptic generic TaskNotFound (-32000) it used to be.
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<HitlDecision>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let handler = HitlRespondHandler {
        pending_approvals: pending.clone(),
        authority: test_authority(),
        shim_trust: Default::default(),
    };
    let err = handler
        .handle(
            Some(json!({"hitl_id": "long-gone", "allow": true, "approval_token": TEST_TOKEN})),
            &crate::protocol::a2a_server::RequestContext::none(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), -32012);
    let msg = err.to_string();
    assert!(msg.contains("approval expired"), "{msg}");
    assert!(msg.contains("auto-denied at timeout"), "{msg}");
}

#[tokio::test]
async fn hitl_respond_no_reason() {
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<HitlDecision>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = oneshot::channel::<HitlDecision>();
    pending.lock().await.insert("test-id".to_string(), tx);

    let handler = HitlRespondHandler {
        pending_approvals: pending.clone(),
        authority: test_authority(),
        shim_trust: Default::default(),
    };
    let result = handler
        .handle(
            Some(json!({"hitl_id": "test-id", "allow": false})),
            &crate::protocol::a2a_server::RequestContext::none(),
        )
        .await;
    assert!(result.is_ok());

    let decision = rx.await.expect("sender dropped");
    assert!(!decision.allow);
    assert!(decision.reason.is_none());
}

#[tokio::test]
async fn hitl_respond_unknown_id_returns_error() {
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<HitlDecision>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let handler = HitlRespondHandler {
        pending_approvals: pending.clone(),
        authority: test_authority(),
        shim_trust: Default::default(),
    };
    let result = handler
        .handle(
            Some(json!({"hitl_id": "no-such-id", "allow": false})),
            &crate::protocol::a2a_server::RequestContext::none(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn hitl_timeout_auto_denies() {
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<HitlDecision>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = oneshot::channel::<HitlDecision>();
    pending.lock().await.insert("timeout-id".to_string(), tx);

    let decision = tokio::time::timeout(Duration::from_millis(100), rx).await;
    assert!(decision.is_err(), "should have timed out");
}

/// §6: a profile with the old caps starts, warns once per key, and the
/// value has no effect (the runner no longer has a setter to receive it).
#[test]
fn stale_caps_warn_once_and_name_the_replacement() {
    let mut h = mur_common::agent::HitlConfig::default();
    assert!(super::stale_cap_warnings(&h).is_empty());
    h.max_iterations = Some(800);
    h.max_tokens = Some(1_000_000);
    let w = super::stale_cap_warnings(&h);
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(
        w[0].contains("hitl.max_iterations: 800") && w[0].contains("IGNORED since 2.79"),
        "{}",
        w[0]
    );
    assert!(
        w[1].contains("hitl.max_tokens") && w[1].contains("mur limits"),
        "{}",
        w[1]
    );
}
