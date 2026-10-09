//! Who may answer a HITL gate, and which questions may be answered.
//!
//! A verified signature proves who WROTE an event, not that the writer may
//! decide. Every agent can write `channels/` and each can sign as itself, so
//! `channel_verify::verify_event` — which checks an event against its own
//! actor's key and, with `MUR_CHANNEL_REQUIRE_SIG` unset, accepts an unsigned
//! one — let an agent approve its own gated action. Approvals, and the
//! requests they answer, are held to a stricter rule that does not depend on
//! that variable: only the router speaks for the human.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::hitl::HitlRequest;

/// True if the router wrote `ev`: a Human or System actor (an agent never
/// speaks for the human), a signature, and that signature verifying against
/// the router's key.
///
/// This proves MUR asked a question or recorded an answer. It does NOT prove a
/// human decided anything: the gate's own `--yes` / tier-grant audit record is
/// router-signed too, as `System`. Use [`is_human_authority`] for that.
///
/// No `require_sig` fallback, on purpose. An unsigned event is exactly what a
/// sandboxed `mur channel approve` writes — it cannot read the router key — so
/// tolerating unsigned approvals is tolerating agent-written ones. The cost: a
/// home with no router identity cannot approve anything.
pub fn is_router_signed(mur_home: &Path, channel_id: &str, ev: &ChannelEvent) -> bool {
    if matches!(ev.actor, ChannelActor::Agent { .. }) || ev.sig.is_none() {
        return false;
    }
    crate::channel_verify::actor_pubkey(mur_home, &ev.actor, ev.key_version)
        .is_some_and(|pk| mur_channel::sign::verify_one(channel_id, ev, &pk, true))
}

/// True if `ev` is the human's decision: router-signed AND spoken as `Human`
/// (`mur channel approve`, the phone).
///
/// A `System` answer is the gate recording what it did on its own — `--yes`
/// or a pre-approved tier. That record is audit, not consent: counting it
/// would let one `--yes` run approve every later run of the same action for
/// the whole approval TTL, with or without `--yes` (#1764).
pub fn is_human_authority(mur_home: &Path, channel_id: &str, ev: &ChannelEvent) -> bool {
    matches!(ev.actor, ChannelActor::Human { .. }) && is_router_signed(mur_home, channel_id, ev)
}

/// The request `hitl_id` names, if the router asked it.
///
/// A response echoes its request's `action_hash`, and the gate releases on
/// that hash. Answering a request an agent wrote would let it put a harmless
/// `summary` in front of the hash of something else. The newest router-signed
/// request with this id wins; one the router did not sign is never answered.
pub fn request_to_answer(
    mur_home: &Path,
    channel_id: &str,
    events: &[ChannelEvent],
    hitl_id: &str,
) -> Result<HitlRequest> {
    let mut unsigned_match = false;
    for e in events
        .iter()
        .rev()
        .filter(|e| e.kind == EventKind::HitlRequest)
    {
        let Ok(r) = serde_json::from_value::<HitlRequest>(e.payload.clone()) else {
            continue;
        };
        if r.hitl_id != hitl_id {
            continue;
        }
        if is_router_signed(mur_home, channel_id, e) {
            return Ok(r);
        }
        unsigned_match = true;
    }
    if unsigned_match {
        bail!(
            "HitlRequest {hitl_id} in channel {channel_id} is not signed by MUR's router — \
             refusing to answer a question MUR did not ask"
        );
    }
    bail!("no pending HitlRequest {hitl_id} in channel {channel_id}")
}

#[cfg(test)]
mod tests {
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
            "agent_id": "mur", "timeout_ms": 1000u64, "summary": "echo hi"
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
        let ch = svc.create_for_workflow("g").unwrap();
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
            assert!(is_router_signed(tmp.path(), &ch.id, &ev), "{actor:?}");
            assert_eq!(
                is_human_authority(tmp.path(), &ch.id, &ev),
                human,
                "{actor:?}"
            );
        }
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
        let ch = svc.create_for_workflow("g").unwrap();

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
                !is_router_signed(tmp.path(), &ch.id, ev)
                    && !is_human_authority(tmp.path(), &ch.id, ev),
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
        let ch = svc.create_for_workflow("g").unwrap();
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
}
