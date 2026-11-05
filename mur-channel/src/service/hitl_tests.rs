//! `channels.hitl_pending` is derived per `hitl_id`, with the same rule as
//! `mur channel pending`: a gate is open while a request has no response
//! naming its id.

use super::*;
use tempfile::TempDir;

/// A payload that parses as `mur_common::hitl::HitlRequest` — the index keys
/// gates by its `hitl_id`, and skips payloads that do not parse, exactly as
/// `mur channel pending` does.
fn hitl_request(hitl_id: &str) -> serde_json::Value {
    serde_json::json!({
        "hitl_id": hitl_id,
        "action_hash": format!("hash-{hitl_id}"),
        "tier": "write",
        "tool_name": "shell",
        "tool_input": {},
        "step_or_call_id": "call-1",
        "agent_id": "mur",
        "timeout_ms": 0,
        "summary": "run it",
    })
}

fn hitl_response(hitl_id: &str) -> serde_json::Value {
    serde_json::json!({
        "hitl_id": hitl_id,
        "action_hash": format!("hash-{hitl_id}"),
        "allow": true,
        "surface": "cli",
    })
}

fn hitl_pending(svc: &ChannelService, ch_id: &str) -> bool {
    svc.index()
        .list(10)
        .unwrap()
        .into_iter()
        .find(|r| r.id == ch_id)
        .unwrap()
        .hitl_pending
}

#[test]
fn answering_one_of_two_open_gates_keeps_the_channel_pending() {
    // Home's "Needs You" reads this flag. The old fold cleared it on any
    // response, so answering gate A hid gate B while B was still waiting.
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();
    for id in ["a", "b"] {
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::HitlRequest,
            hitl_request(id),
            None,
        )
        .unwrap();
    }
    svc.append(
        &ch.id,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        hitl_response("a"),
        None,
    )
    .unwrap();
    assert!(hitl_pending(&svc, &ch.id), "gate b is still waiting");

    svc.append(
        &ch.id,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        hitl_response("b"),
        None,
    )
    .unwrap();
    assert!(!hitl_pending(&svc, &ch.id), "every gate answered");
}

#[test]
fn a_response_for_an_unknown_gate_does_not_clear_a_pending_one() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();
    svc.append(
        &ch.id,
        ChannelActor::System,
        EventKind::HitlRequest,
        hitl_request("a"),
        None,
    )
    .unwrap();
    svc.append(
        &ch.id,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        hitl_response("stale"),
        None,
    )
    .unwrap();
    assert!(hitl_pending(&svc, &ch.id));
}

#[test]
fn a_response_folded_before_its_request_still_closes_the_gate() {
    // Cross-process folds can arrive out of order (see `record_event`). The
    // per-gate bits make the result independent of fold order.
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();
    let ev = |seq: u64, kind: EventKind, payload: serde_json::Value| ChannelEvent {
        seq,
        ts: chrono::Utc::now(),
        actor: ChannelActor::System,
        kind,
        payload,
        idempotency_key: None,
        sig: None,
        key_version: None,
        prev_hash: None,
    };
    let idx = svc.index();
    idx.record_event(&ch.id, &ev(1, EventKind::HitlResponse, hitl_response("a")))
        .unwrap();
    idx.record_event(&ch.id, &ev(0, EventKind::HitlRequest, hitl_request("a")))
        .unwrap();
    assert!(!hitl_pending(&svc, &ch.id));
}

#[test]
fn rebuild_rederives_pending_per_gate() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();
    for id in ["a", "b"] {
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::HitlRequest,
            hitl_request(id),
            None,
        )
        .unwrap();
    }
    svc.append(
        &ch.id,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        hitl_response("a"),
        None,
    )
    .unwrap();
    svc.index().rebuild_from(svc.store()).unwrap();
    assert!(hitl_pending(&svc, &ch.id), "rebuild must not lose gate b");
}

#[test]
fn hitl_request_sets_pending_and_response_clears_it() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();

    svc.append(
        &ch.id,
        ChannelActor::System,
        EventKind::HitlRequest,
        hitl_request("h1"),
        None,
    )
    .unwrap();
    let row = |svc: &ChannelService| {
        svc.index()
            .list(10)
            .unwrap()
            .into_iter()
            .find(|r| r.id == ch.id)
            .unwrap()
    };
    assert!(row(&svc).hitl_pending);

    svc.append(
        &ch.id,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        hitl_response("h1"),
        None,
    )
    .unwrap();
    assert!(!row(&svc).hitl_pending);
}
