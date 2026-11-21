use super::*;
use mur_channel::ChannelService;
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

fn response(hitl_id: &str) -> serde_json::Value {
    serde_json::json!({
        "hitl_id": hitl_id, "action_hash": "AH", "allow": true,
        "reason": "", "surface": "cli"
    })
}

fn request(hitl_id: &str, hash: &str) -> serde_json::Value {
    serde_json::json!({
        "hitl_id": hitl_id, "action_hash": hash, "tier": "destructive",
        "tool_name": "bash", "tool_input": {}, "step_or_call_id": "s0",
        "agent_id": "mur", "timeout_ms": 1000u64, "summary": "echo hi",
        "issued_at": chrono::Utc::now()
    })
}

/// The router writes as the human (`mur channel approve`, the phone) or as
/// the system (the gate's own `--yes` / tier-grant audit record). Both are
/// the router's; only the human's is a decision.
#[test]
fn only_the_routers_human_answer_is_the_humans_decision() {
    let tmp = TempDir::new().unwrap();
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc
        .create_for_workflow("g", mur_channel::service::ChannelOwner::Legacy)
        .unwrap();
    for (actor, human) in [
        (ChannelActor::local_human(), true),
        (ChannelActor::System, false),
    ] {
        let ev = svc
            .append_signed(
                &ch.id,
                &router,
                0,
                actor.clone(),
                EventKind::HitlResponse,
                response("h1"),
                None,
            )
            .unwrap();
        assert!(
            is_control_plane_signed(&AuthorityCtx::load(tmp.path(), &ch.id).unwrap(), &ev),
            "{actor:?}"
        );
        assert_eq!(
            is_human_authority(&AuthorityCtx::load(tmp.path(), &ch.id).unwrap(), &ev),
            human,
            "{actor:?}"
        );
    }
}

/// B2: `is_control_plane_signed` asks only "did the router write this". A
/// `Human` actor that names a key must not move the check onto the human
/// chain: the router's signature still passes, a human-key signature does
/// not (that is C3's question, not this one's).
#[test]
fn is_control_plane_signed_unchanged_for_human_actor() {
    let tmp = TempDir::new().unwrap();
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc
        .create_for_workflow("g", mur_channel::service::ChannelOwner::Legacy)
        .unwrap();
    let human = AgentIdentity::generate();
    let actor = ChannelActor::local_human_with_key(human.pubkey_text());
    let by_router = svc
        .append_signed(
            &ch.id,
            &router,
            0,
            actor.clone(),
            EventKind::HitlResponse,
            response("h1"),
            None,
        )
        .unwrap();
    assert!(is_control_plane_signed(
        &AuthorityCtx::load(tmp.path(), &ch.id).unwrap(),
        &by_router
    ));
    let by_human = svc
        .append_signed(
            &ch.id,
            &human,
            0,
            actor,
            EventKind::HitlResponse,
            response("h2"),
            None,
        )
        .unwrap();
    assert!(!is_control_plane_signed(
        &AuthorityCtx::load(tmp.path(), &ch.id).unwrap(),
        &by_human
    ));
}

/// A human answer the router signed for one channel does not carry over
/// to another: the channel id is in the signed input, so copying the
/// event into a second channel's log is not a decision there (#1764).
#[test]
fn a_human_answer_signed_for_another_channel_is_not_authority() {
    let tmp = TempDir::new().unwrap();
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let here = svc
        .create_for_workflow("here", mur_channel::service::ChannelOwner::Legacy)
        .unwrap();
    let there = svc
        .create_for_workflow("there", mur_channel::service::ChannelOwner::Legacy)
        .unwrap();
    let ev = svc
        .append_signed(
            &here.id,
            &router,
            0,
            ChannelActor::local_human(),
            EventKind::HitlResponse,
            response("h1"),
            None,
        )
        .unwrap();
    assert!(
        is_human_authority(&AuthorityCtx::load(tmp.path(), &here.id).unwrap(), &ev),
        "precondition"
    );
    assert!(
        !is_control_plane_signed(&AuthorityCtx::load(tmp.path(), &there.id).unwrap(), &ev)
            && !is_human_authority(&AuthorityCtx::load(tmp.path(), &there.id).unwrap(), &ev),
        "a signature for {} must not verify in {}",
        here.id,
        there.id
    );
}

/// The self-approval this module exists to stop. Each of these passes
/// `verify_event` with enforcement off (the default), and none of them
/// is the human's answer.
#[test]
fn an_agent_cannot_answer_for_the_human() {
    let tmp = TempDir::new().unwrap();
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let qa = crate::channel_writer::plant_identity_for(tmp.path(), "qa");
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc
        .create_for_workflow("g", mur_channel::service::ChannelOwner::Legacy)
        .unwrap();

    // 1. Unsigned, claiming to be the human — what a sandboxed
    //    `mur channel approve` writes, since it cannot read the router key.
    let unsigned = svc
        .append(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::HitlResponse,
            response("h1"),
            None,
        )
        .unwrap();
    // 2. An agent answering as itself, correctly signed with its own key.
    let own_key = svc
        .append_signed(
            &ch.id,
            &qa,
            0,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::HitlResponse,
            response("h1"),
            None,
        )
        .unwrap();
    // 3. The router's key, but speaking as an agent (the runtime's own
    //    chat-gate memory is written this way) — an agent, not the human.
    let router_as_agent = svc
        .append_signed(
            &ch.id,
            &router,
            0,
            ChannelActor::Agent { id: "mur".into() },
            EventKind::HitlResponse,
            response("h1"),
            None,
        )
        .unwrap();
    // 4. Claiming the human, signed by a key that is not the router's.
    let wrong_key = svc
        .append_signed(
            &ch.id,
            &AgentIdentity::generate(),
            0,
            ChannelActor::local_human(),
            EventKind::HitlResponse,
            response("h1"),
            None,
        )
        .unwrap();

    assert!(
        crate::channel_verify::verify_event(tmp.path(), &ch.id, &unsigned, false)
            && crate::channel_verify::verify_event(tmp.path(), &ch.id, &own_key, false),
        "precondition: per-actor verification alone accepts these"
    );
    for (what, ev) in [
        ("unsigned human", &unsigned),
        ("agent's own key", &own_key),
        ("router key as agent", &router_as_agent),
        ("wrong key as human", &wrong_key),
    ] {
        assert!(
            !is_control_plane_signed(&AuthorityCtx::load(tmp.path(), &ch.id).unwrap(), ev)
                && !is_human_authority(&AuthorityCtx::load(tmp.path(), &ch.id).unwrap(), ev),
            "{what} must not carry the router's authority"
        );
    }
}

/// An agent-written request is not answered, and cannot shadow the real
/// one by reusing its id: the older router-signed request still wins.
#[test]
fn only_a_router_signed_request_is_answered() {
    let tmp = TempDir::new().unwrap();
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc
        .create_for_workflow("g", mur_channel::service::ChannelOwner::Legacy)
        .unwrap();
    svc.append(
        &ch.id,
        ChannelActor::System,
        EventKind::HitlRequest,
        request("forged", "HASH-OF-RM-RF"),
        None,
    )
    .unwrap();
    svc.append_signed(
        &ch.id,
        &router,
        0,
        ChannelActor::System,
        EventKind::HitlRequest,
        request("h1", "REAL"),
        None,
    )
    .unwrap();
    svc.append(
        &ch.id,
        ChannelActor::System,
        EventKind::HitlRequest,
        request("h1", "HASH-OF-RM-RF"),
        None,
    )
    .unwrap();
    let events = svc.load_events(&ch.id).unwrap();

    let err = request_to_answer(tmp.path(), &ch.id, &events, "forged").unwrap_err();
    assert!(err.to_string().contains("not signed"), "{err}");
    let r = request_to_answer(tmp.path(), &ch.id, &events, "h1").unwrap();
    assert_eq!(r.action_hash, "REAL", "the shadowing copy must be skipped");
    let err = request_to_answer(tmp.path(), &ch.id, &events, "nope").unwrap_err();
    assert!(err.to_string().contains("no pending"), "{err}");
}
