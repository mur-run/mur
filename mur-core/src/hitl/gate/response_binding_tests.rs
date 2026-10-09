//! A response settles a gate only as the answer to its own request (#1764,
//! fix items 2 and 3 as ruled in the issue thread).
//!
//! Approval of an *action* is reused for `APPROVAL_TTL_SECS` from the
//! response — that is the overnight case and stays. What is void is a
//! response that answers no router-signed request, answers a different
//! action than its request names, answers a request that was re-issued under
//! the same `hitl_id`, or lands after its request expired.

use super::*;
use tempfile::TempDir;

fn action(cmd: &str) -> ActionRequest {
    ActionRequest {
        tier: RiskTier::Destructive,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({ "cmd": cmd }),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        summary: cmd.into(),
    }
}

/// Nobody is watching: park and report blocked.
fn unattended() -> GatePolicy {
    GatePolicy {
        yes: false,
        unanswered: Unanswered::Defer,
        auto_approve_tiers: vec![],
    }
}

/// A home with a router identity and one workflow channel.
fn setup() -> (TempDir, String) {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();
    (tmp, ch.id)
}

/// Run the gate unattended for `a` and return the parked request's
/// `(hitl_id, action_hash)`.
async fn park(home: &Path, ch: &str, a: &ActionRequest) -> (String, String) {
    let d = gate(home, ch, a, &unattended(), None, None).await.unwrap();
    assert!(!d.allow && d.deferred, "precondition: parked: {d:?}");
    (d.hitl_id.expect("parked id"), d.action_hash)
}

/// Append a router-signed event, as the router would.
fn append_router(
    home: &Path,
    ch: &str,
    actor: ChannelActor,
    kind: EventKind,
    payload: serde_json::Value,
) {
    let svc = ChannelService::open(home).unwrap();
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        ch,
        ROUTER_AGENT,
        actor,
        kind,
        payload,
        None,
    )
    .unwrap();
}

/// The human approves `hitl_id` for `hash` — router-signed, `Human` actor.
fn human_allows(home: &Path, ch: &str, hitl_id: &str, hash: &str) {
    let resp = HitlResponse {
        hitl_id: hitl_id.into(),
        action_hash: hash.into(),
        allow: true,
        reason: "test".into(),
        surface: "cli".into(),
    };
    append_router(
        home,
        ch,
        ChannelActor::Human {
            name: "user".into(),
        },
        EventKind::HitlResponse,
        serde_json::to_value(&resp).unwrap(),
    );
}

/// A router-signed `HitlRequest` for `hitl_id`, as the gate writes it.
fn router_request(home: &Path, ch: &str, hitl_id: &str, hash: &str) {
    let q = HitlRequest {
        hitl_id: hitl_id.into(),
        action_hash: hash.into(),
        tier: RiskTier::Destructive,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({}),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        timeout_ms: 0,
        summary: "x".into(),
    };
    append_router(
        home,
        ch,
        ChannelActor::System,
        EventKind::HitlRequest,
        serde_json::to_value(&q).unwrap(),
    );
}

/// Move the `HitlRequest` for `hitl_id` back in time by `age`.
///
/// `ts` is store-assigned and outside the signature (`ChannelEvent::sig`), so
/// rewriting it leaves the event verifiable — exactly the property a backdated
/// request has in a real log that is simply old.
fn age_request(home: &Path, ch: &str, hitl_id: &str, age: chrono::Duration) {
    let svc = ChannelService::open(home).unwrap();
    let path = svc.store().events_path(ch);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut out = String::new();
    for line in text.lines() {
        let mut ev: mur_common::channel::ChannelEvent = serde_json::from_str(line).unwrap();
        if ev.kind == EventKind::HitlRequest
            && ev.payload.get("hitl_id").and_then(|v| v.as_str()) == Some(hitl_id)
        {
            ev.ts -= age;
        }
        out.push_str(&serde_json::to_string(&ev).unwrap());
        out.push('\n');
    }
    std::fs::write(&path, out).unwrap();
}

fn ttl() -> chrono::Duration {
    chrono::Duration::seconds(mur_common::hitl::APPROVAL_TTL_SECS)
}

// ── Controls: what must keep working ──────────────────────────────────

/// The overnight case: a human answers run 1's request, run 2 of the same
/// action — a new `hitl_id` — is released by that answer.
#[tokio::test]
async fn a_new_run_of_an_approved_action_reuses_the_approval() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (id, hash) = park(tmp.path(), &ch, &a).await;
    human_allows(tmp.path(), &ch, &id, &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(d.allow, "a re-run is a new instance, not a re-issue: {d:?}");
}

/// A late but in-time answer (day 6 of 7) still counts.
#[tokio::test]
async fn an_answer_inside_the_request_window_counts() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (id, hash) = park(tmp.path(), &ch, &a).await;
    age_request(tmp.path(), &ch, &id, ttl() - chrono::Duration::hours(1));
    human_allows(tmp.path(), &ch, &id, &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(d.allow, "answered before the request expired: {d:?}");
}

// ── Binding (fix item 2) ──────────────────────────────────────────────

/// An answer whose `hitl_id` names no request settles nothing.
#[tokio::test]
async fn an_answer_to_no_request_does_not_settle() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (id, hash) = park(tmp.path(), &ch, &a).await;
    human_allows(tmp.path(), &ch, "hitl-ghost", &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "no request named hitl-ghost: {d:?}");
    assert_eq!(
        d.hitl_id.as_deref(),
        Some(id.as_str()),
        "the real request is still pending"
    );
}

/// An answer that precedes its request answers nothing: nobody had been
/// asked yet.
#[tokio::test]
#[ignore = "needs signed issued_at in the HITL payload: #1764 option C, not yet implemented"]
async fn an_answer_before_its_request_does_not_settle() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (_, hash) = park(tmp.path(), &ch, &a).await;
    human_allows(tmp.path(), &ch, "hitl-early", &hash);
    router_request(tmp.path(), &ch, "hitl-early", &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "answered before it was asked: {d:?}");
}

/// The answer's `action_hash` must be its request's. Otherwise approving a
/// harmless request could carry the hash of something else.
#[tokio::test]
async fn an_answer_whose_hash_differs_from_its_request_does_not_settle() {
    let (tmp, ch) = setup();
    let harmless = action("ls");
    let target = action("rm -rf build");
    let (harmless_id, _) = park(tmp.path(), &ch, &harmless).await;
    let (_, target_hash) = park(tmp.path(), &ch, &target).await;
    human_allows(tmp.path(), &ch, &harmless_id, &target_hash);

    let d = gate(tmp.path(), &ch, &target, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "the human saw `ls`, not `rm`: {d:?}");
}

// ── Re-issue and expiry (fix item 3) ──────────────────────────────────

/// A second router-signed request under the same `hitl_id` is a bug or a
/// forgery; every answer to that id is void.
#[tokio::test]
async fn a_reissued_hitl_id_voids_its_answers() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (id, hash) = park(tmp.path(), &ch, &a).await;
    human_allows(tmp.path(), &ch, &id, &hash);
    router_request(tmp.path(), &ch, &id, &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "re-issued id: {d:?}");
    assert_ne!(
        d.hitl_id.as_deref(),
        Some(id.as_str()),
        "a poisoned id is never offered for answering again"
    );
}

/// An answer that lands after its request expired does not count.
#[tokio::test]
#[ignore = "needs signed issued_at in the HITL payload: #1764 option C, not yet implemented"]
async fn an_answer_after_the_request_expired_does_not_settle() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (id, hash) = park(tmp.path(), &ch, &a).await;
    age_request(tmp.path(), &ch, &id, ttl() + chrono::Duration::hours(1));
    human_allows(tmp.path(), &ch, &id, &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow && d.deferred, "request had expired: {d:?}");
    assert_ne!(d.hitl_id.as_deref(), Some(id.as_str()), "asks afresh");
}
