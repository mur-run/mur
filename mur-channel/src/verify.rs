//! Per-actor verify-on-fold (v3d-2): each event is verified against ITS actor's
//! pubkey (`<mur_home>/agents/<id>`), not a single channel writer.
use std::path::{Path, PathBuf};

use mur_common::channel::{ChannelActor, ChannelEvent};

/// The channel's router/owner agent — the trusted WRITER that signs the events
/// appended to workflow/HITL channels (v3d). The concierge `"mur"` owns these
/// channels; its on-disk identity (`<home>/agents/mur/`) signs `HitlRequest`/
/// `HitlResponse`/workflow events, and the HITL gate verifies an incoming
/// `HitlResponse` against this agent's pubkey before releasing. Internal
/// directory slug, so lowercase (matches the on-disk `name`).
pub const ROUTER_AGENT: &str = "mur";

/// The directory holding the key that should have signed `actor`'s events.
/// Agent{id} → that agent's home; System/Human → the router ("mur") which
/// writes them.
///
/// `None` when the id is not a valid agent name. The id comes from the event
/// itself, and every agent can write `channels/`, so an unchecked join is a
/// traversal primitive: `Agent{id: "../channels/<dir>"}` would point
/// verification at a key the writer planted next to its own event.
pub fn actor_key_dir(mur_home: &Path, actor: &ChannelActor) -> Option<PathBuf> {
    let agent = match actor {
        ChannelActor::Agent { id } => id.as_str(),
        _ => ROUTER_AGENT,
    };
    mur_common::validate_agent_name(agent).ok()?;
    Some(mur_home.join("agents").join(agent))
}

/// Resolve the pubkey that should have signed `actor`'s events. `None` when
/// the key cannot be read, or when the actor id is not a valid agent name
/// (see [`actor_key_dir`]).
pub fn actor_pubkey(
    mur_home: &Path,
    actor: &ChannelActor,
    key_version: Option<u32>,
) -> Option<[u8; 32]> {
    crate::sign::resolve_writer_pubkey(&actor_key_dir(mur_home, actor)?, key_version)
}

/// True if `ev` verifies against its actor's key (present sig must verify;
/// missing sig tolerated iff `!require_sig`).
///
/// An **unresolvable key is not the same as an absent signature.** Tolerating
/// a missing signature is the migration-safety rule for legacy unsigned events
/// (v3d). An event that DOES carry a signature but whose key cannot be read has
/// not been verified by anything — accepting it would let an unreadable key
/// directory switch verification off silently. The Linux sandbox grants
/// `agents/` for reading as a whole, so a peer created after the seal is
/// readable there; a key can still be missing, deleted, or unreadable for
/// other reasons, and that must never read as "unsigned". It fails closed
/// instead.
///
/// Not an authorization check: it proves who wrote `ev`, not that the writer
/// may decide anything. HITL approvals use `hitl::authority` instead.
pub fn verify_event(
    mur_home: &Path,
    channel_id: &str,
    ev: &ChannelEvent,
    require_sig: bool,
) -> bool {
    verify_with_actor_key(
        mur_home,
        channel_id,
        &ev.actor,
        ev.key_version,
        ev.sig.is_some(),
        require_sig,
        |pk| crate::sign::verify_one(channel_id, ev, pk, require_sig),
    )
}

/// [`verify_event`] for a line whose `kind` this binary does not know
/// (#2287): same key resolution, same fail-closed rules.
pub fn verify_newer_kind_event(
    mur_home: &Path,
    channel_id: &str,
    ev: &crate::store::NewerKindEvent,
    require_sig: bool,
) -> bool {
    verify_with_actor_key(
        mur_home,
        channel_id,
        &ev.actor,
        ev.key_version,
        ev.sig.is_some(),
        require_sig,
        |pk| crate::sign::verify_newer_kind(channel_id, ev, pk, require_sig),
    )
}

fn verify_with_actor_key(
    mur_home: &Path,
    channel_id: &str,
    actor: &ChannelActor,
    key_version: Option<u32>,
    signed: bool,
    require_sig: bool,
    verify: impl FnOnce(&[u8; 32]) -> bool,
) -> bool {
    match actor_pubkey(mur_home, actor, key_version) {
        Some(pk) => verify(&pk),
        // Unsigned: the legacy-tolerance rule still applies — there is nothing
        // a key would have checked.
        None if !signed => !require_sig,
        None => {
            tracing::warn!(
                channel_id,
                actor = ?actor,
                key_version = ?key_version,
                "event carries a signature but its actor's public key could not \
                 be read — treating as UNVERIFIED (an unreadable key must not \
                 pass as an absent signature)"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::EventKind;
    use mur_common::identity::AgentIdentity;

    /// A SIGNED event whose actor key cannot be read must not pass, even with
    /// enforcement off. Otherwise an unreadable key directory — missing,
    /// deleted, or outside a sandbox's read grants — silently turns signature
    /// verification into a no-op, and a forged event is indistinguishable from
    /// a genuine one.
    #[test]
    fn a_signed_event_with_an_unreadable_key_does_not_verify() {
        let tmp = tempfile::TempDir::new().unwrap();
        let qa_home = tmp.path().join("agents").join("qa");
        std::fs::create_dir_all(&qa_home).unwrap();
        let qa = AgentIdentity::generate();
        qa.save(&qa_home).unwrap();
        let svc = crate::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        svc.append_signed(
            &ch.id,
            &qa,
            0,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::Message,
            serde_json::json!({"text":"hi"}),
            None,
        )
        .unwrap();
        let ev = svc.load_events(&ch.id).unwrap().pop().unwrap();
        assert!(ev.sig.is_some(), "precondition: the event is signed");
        // Sanity: it verifies while the key is readable, with enforcement OFF.
        assert!(verify_event(tmp.path(), &ch.id, &ev, false));

        // Now make the key unresolvable, exactly as a sandbox would.
        std::fs::remove_dir_all(&qa_home).unwrap();

        assert!(
            !verify_event(tmp.path(), &ch.id, &ev, false),
            "a signed event whose key cannot be read must NOT count as verified"
        );
    }

    /// ...while an UNSIGNED event keeps the migration-safety rule: there is no
    /// signature for a key to have checked, so `require_sig` still decides.
    #[test]
    fn an_unsigned_event_still_follows_require_sig() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = crate::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("ghost").unwrap();
        svc.append_signed(
            &ch.id,
            &AgentIdentity::generate(),
            0,
            ChannelActor::Agent { id: "ghost".into() },
            EventKind::Message,
            serde_json::json!({"text":"legacy"}),
            None,
        )
        .unwrap();
        let mut ev = svc.load_events(&ch.id).unwrap().pop().unwrap();
        ev.sig = None; // legacy, pre-v3d

        assert!(
            verify_event(tmp.path(), &ch.id, &ev, false),
            "unsigned + enforcement off = tolerated"
        );
        assert!(
            !verify_event(tmp.path(), &ch.id, &ev, true),
            "unsigned + enforcement on = rejected"
        );
    }

    /// An actor id is data the event's writer chose, and every agent can
    /// write `channels/`. A traversal id must not reach a key planted there:
    /// the event is signed, so an unresolvable key fails closed — even with
    /// enforcement off, which is the default.
    #[test]
    fn a_traversal_actor_id_cannot_point_at_a_planted_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = crate::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        // The attacker's key, planted inside the world-writable channel store.
        let planted = tmp.path().join("channels").join("planted");
        std::fs::create_dir_all(&planted).unwrap();
        let attacker = AgentIdentity::generate();
        attacker.save(&planted).unwrap();
        // A real home always has `agents/`; `agents/..` only resolves if it does.
        std::fs::create_dir_all(tmp.path().join("agents")).unwrap();
        let actor = ChannelActor::Agent {
            id: "../channels/planted".into(),
        };
        // Precondition: without the check, this join WOULD resolve the key.
        assert!(
            crate::sign::resolve_writer_pubkey(
                &tmp.path().join("agents").join("../channels/planted"),
                None
            )
            .is_some(),
            "precondition: the raw join reaches the planted key"
        );
        svc.append_signed(
            &ch.id,
            &attacker,
            0,
            actor.clone(),
            EventKind::HitlResponse,
            serde_json::json!({"hitl_id":"h1","action_hash":"x","allow":true}),
            None,
        )
        .unwrap();
        let ev = svc.load_events(&ch.id).unwrap().pop().unwrap();
        assert!(ev.sig.is_some(), "precondition: the event is signed");

        assert_eq!(actor_key_dir(tmp.path(), &actor), None);
        assert_eq!(actor_pubkey(tmp.path(), &actor, None), None);
        assert!(
            !verify_event(tmp.path(), &ch.id, &ev, false),
            "a traversal actor id must not verify, even with enforcement off"
        );
    }

    /// The check rejects what `mur agent create` rejects, and nothing it
    /// accepts — a real agent's events must keep verifying.
    #[test]
    fn actor_key_dir_accepts_exactly_valid_agent_names() {
        let home = Path::new("/h");
        for bad in ["", "..", "../x", "a/b", "a\\b", "-flag", "x/../y", "a b"] {
            let actor = ChannelActor::Agent { id: bad.into() };
            assert_eq!(actor_key_dir(home, &actor), None, "{bad:?} must be refused");
        }
        for good in ["qa", "mur", "agent-1", "support_bot"] {
            let actor = ChannelActor::Agent { id: good.into() };
            assert_eq!(
                actor_key_dir(home, &actor),
                Some(home.join("agents").join(good))
            );
        }
        // System and Human resolve to the router, which is always valid.
        assert_eq!(
            actor_key_dir(home, &ChannelActor::System),
            Some(home.join("agents").join(ROUTER_AGENT))
        );
    }

    #[test]
    fn event_verifies_against_its_own_actor_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let qa_home = tmp.path().join("agents").join("qa");
        std::fs::create_dir_all(&qa_home).unwrap();
        let qa = AgentIdentity::generate();
        qa.save(&qa_home).unwrap();
        let svc = crate::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        svc.append_signed(
            &ch.id,
            &qa,
            0,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::Message,
            serde_json::json!({"text":"hi"}),
            None,
        )
        .unwrap();
        let ev = svc.load_events(&ch.id).unwrap().pop().unwrap();
        assert!(
            verify_event(tmp.path(), &ch.id, &ev, true),
            "qa-signed event verifies vs qa's key"
        );
        let imposter = AgentIdentity::generate();
        let forged_sig =
            crate::sign::sign_event(&imposter, &ch.id, &ev.actor, ev.kind, &ev.payload, None);
        let mut forged = ev.clone();
        forged.sig = Some(forged_sig);
        assert!(
            !verify_event(tmp.path(), &ch.id, &forged, true),
            "imposter sig rejected"
        );
    }
}
