//! Per-actor verify-on-fold (v3d-2): each event is verified against ITS actor's
//! pubkey (`<mur_home>/agents/<id>`), not a single channel writer.
use std::path::{Path, PathBuf};

use mur_common::channel::{ChannelActor, ChannelEvent};

pub mod human;

/// The control-plane identity — the trusted WRITER that signs the events
/// appended to workflow/HITL channels (v3d), and the trust root a reader checks
/// them against. Not a fleet's `router:` member, which `fleet.yaml` chooses and
/// which is therefore never trusted to widen bounds. The concierge `"mur"` owns
/// these channels; its on-disk identity (`<home>/agents/mur/`) signs `HitlRequest`/
/// `HitlResponse`/workflow events, and the HITL gate verifies an incoming
/// `HitlResponse` against this agent's pubkey before releasing. Internal
/// directory slug, so lowercase (matches the on-disk `name`).
pub const CONTROL_PLANE_AGENT: &str = "mur";

/// Whose identity signs an event, and so whose key a reader checks it against.
///
/// The writer (`mur_core::channel_writer`) and the verifier
/// ([`actor_key_dir`]) both resolve a key directory through this one type, so
/// the two cannot name the trust root differently (#2081).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signer<'a> {
    /// The control plane: the one identity ([`CONTROL_PLANE_AGENT`]) every reader
    /// checks a `System` or keyless `Human` event against.
    ///
    /// Never a fleet's configured `router:`. That field lives in `fleet.yaml`,
    /// so trusting it would let a fleet's own config choose who may speak as
    /// the system — and a custom router's key writes `System` events its own
    /// channel rejects (#2116, #2072).
    ControlPlane,
    /// A named agent speaking as itself: `ChannelActor::Agent { id }`.
    Agent(&'a str),
}

impl<'a> Signer<'a> {
    /// The signer a reader holds `actor`'s events to. A `Human` actor that
    /// names its own key is resolved on the human chain before this is
    /// consulted (see [`actor_pubkey`]).
    pub fn for_actor(actor: &'a ChannelActor) -> Self {
        match actor {
            ChannelActor::Agent { id } => Signer::Agent(id),
            _ => Signer::ControlPlane,
        }
    }

    /// The agent directory name holding this signer's identity. Not validated:
    /// an `Agent` name taken from an event must go through [`actor_key_dir`].
    pub fn agent_name(self) -> &'a str {
        match self {
            Signer::ControlPlane => CONTROL_PLANE_AGENT,
            Signer::Agent(name) => name,
        }
    }

    /// `<mur_home>/agents/<name>`, where this signer's identity lives. Same
    /// caveat as [`Signer::agent_name`].
    pub fn key_home(self, mur_home: &Path) -> PathBuf {
        mur_home.join("agents").join(self.agent_name())
    }
}

/// An event's actor together with the identity that signs it: what a writer
/// hands to `mur_core::channel_writer`.
///
/// The writer used to take the two separately, and nothing tied them: a
/// `System` actor under a fleet router's key compiled, was written, and was
/// then rejected by every reader (#2072, #2116). Here the signer is derived
/// from the actor ([`Signer::for_actor`]), so a pair a reader would reject
/// cannot be built by accident. The one way to name a different signer is
/// [`Authorship::out_of_role`], which says so at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorship<'a> {
    actor: ChannelActor,
    /// `None` for a matched pair; the signer is then the actor's own.
    out_of_role: Option<Signer<'a>>,
}

impl Authorship<'static> {
    fn matched(actor: ChannelActor) -> Self {
        Self {
            actor,
            out_of_role: None,
        }
    }

    /// A `System` event, signed by the control plane.
    pub fn system() -> Self {
        Self::matched(ChannelActor::System)
    }

    /// `agent` speaking as itself, signed with its own key.
    pub fn agent(agent: &str) -> Self {
        Self::matched(ChannelActor::Agent {
            id: agent.to_string(),
        })
    }

    /// The local human owner without a key of their own on the event
    /// ([`ChannelActor::local_human`]), signed by the control plane.
    pub fn local_human() -> Self {
        Self::matched(ChannelActor::local_human())
    }

    /// The matched pair for `actor`, whatever it is.
    ///
    /// `None` for a `Human` that names its own key: a reader resolves that
    /// one on the human chain ([`actor_pubkey`]), which no [`Signer`] stands
    /// for. Such an event is written by `mur_core::human_writer`.
    pub fn matching(actor: ChannelActor) -> Option<Self> {
        match actor {
            ChannelActor::Human { key: Some(_), .. } => None,
            actor => Some(Self::matched(actor)),
        }
    }
}

impl<'a> Authorship<'a> {
    /// `actor`'s event signed by `signer`, whether or not a reader holds
    /// `actor` to that signer.
    ///
    /// When the two disagree, [`verify_event`] rejects the event. Reach for
    /// this only when another check is the one that matters (the conversation
    /// watermark holds a session's `System` card to the session agent's key),
    /// or in a test that needs a line signed by the wrong identity.
    pub fn out_of_role(signer: Signer<'a>, actor: ChannelActor) -> Self {
        Self {
            actor,
            out_of_role: Some(signer),
        }
    }

    /// The identity that signs this event.
    pub fn signer(&self) -> Signer<'_> {
        match self.out_of_role {
            Some(signer) => signer,
            None => Signer::for_actor(&self.actor),
        }
    }

    pub fn actor(&self) -> &ChannelActor {
        &self.actor
    }

    pub fn into_actor(self) -> ChannelActor {
        self.actor
    }
}

/// The directory holding the key that should have signed `actor`'s events:
/// [`Signer::for_actor`]'s home.
///
/// `None` when the id is not a valid agent name. The id comes from the event
/// itself, and every agent can write `channels/`, so an unchecked join is a
/// traversal primitive: `Agent{id: "../channels/<dir>"}` would point
/// verification at a key the writer planted next to its own event.
pub fn actor_key_dir(mur_home: &Path, actor: &ChannelActor) -> Option<PathBuf> {
    let signer = Signer::for_actor(actor);
    mur_common::validate_agent_name(signer.agent_name()).ok()?;
    Some(signer.key_home(mur_home))
}

/// Resolve the pubkey that should have signed `actor`'s events. `None` when
/// the key cannot be read, or when the actor id is not a valid agent name
/// (see [`actor_key_dir`]).
///
/// A `Human` actor that names its key (at-mention phase 2, D-4) resolves on
/// the local human chain instead, strictly: see [`human::resolve_human_pubkey`].
/// A keyless `Human` is still the router, as before phase 2.
pub fn actor_pubkey(
    mur_home: &Path,
    actor: &ChannelActor,
    key_version: Option<u32>,
) -> Option<[u8; 32]> {
    if let ChannelActor::Human { key: Some(key), .. } = actor {
        return human::resolve_human_pubkey(mur_home, key, key_version);
    }
    crate::sign::resolve_writer_pubkey(&actor_key_dir(mur_home, actor)?, key_version)
}

/// The control-plane key at `key_version`, whatever the event's actor says. For
/// checks whose question is "did the control plane write this" — they must not
/// follow `actor_pubkey` onto the human chain when an actor names a key.
pub fn control_plane_pubkey(mur_home: &Path, key_version: Option<u32>) -> Option<[u8; 32]> {
    crate::sign::resolve_writer_pubkey(&Signer::ControlPlane.key_home(mur_home), key_version)
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
        let ch = svc
            .create_for_agent("qa", crate::service::ChannelOwner::Legacy)
            .unwrap();
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
        let ch = svc
            .create_for_agent("ghost", crate::service::ChannelOwner::Legacy)
            .unwrap();
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
        let ch = svc
            .create_for_agent("qa", crate::service::ChannelOwner::Legacy)
            .unwrap();
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
            Some(home.join("agents").join(CONTROL_PLANE_AGENT))
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
        let ch = svc
            .create_for_agent("qa", crate::service::ChannelOwner::Legacy)
            .unwrap();
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

    /// #2081 item 7: the writer and the verifier resolve a key directory
    /// through the same type. If this mapping moves, both move.
    #[test]
    fn the_verifier_holds_each_actor_to_signer_for_actor() {
        let home = Path::new("/mur-home");
        let agent = ChannelActor::Agent { id: "qa".into() };
        assert_eq!(Signer::for_actor(&agent), Signer::Agent("qa"));
        for actor in [ChannelActor::System, ChannelActor::local_human()] {
            assert_eq!(Signer::for_actor(&actor), Signer::ControlPlane);
        }
        for actor in [agent, ChannelActor::System, ChannelActor::local_human()] {
            assert_eq!(
                actor_key_dir(home, &actor),
                Some(Signer::for_actor(&actor).key_home(home)),
                "{actor:?}"
            );
        }
    }

    /// The control plane is one fixed identity. It is never read from a
    /// fleet's `router:`, which the fleet's own config controls.
    #[test]
    fn the_control_plane_is_the_router_agent_and_nothing_configurable() {
        assert_eq!(Signer::ControlPlane.agent_name(), CONTROL_PLANE_AGENT);
        assert_eq!(
            Signer::ControlPlane.key_home(Path::new("/h")),
            Path::new("/h").join("agents").join(CONTROL_PLANE_AGENT)
        );
        assert_eq!(Signer::Agent("orchestrator").agent_name(), "orchestrator");
    }

    /// An actor id that is not an agent name still resolves no directory:
    /// `Signer::key_home` is unchecked, `actor_key_dir` is the checked door.
    #[test]
    fn actor_key_dir_still_refuses_a_traversal_id() {
        let actor = ChannelActor::Agent {
            id: "../channels/x".into(),
        };
        assert_eq!(actor_key_dir(Path::new("/h"), &actor), None);
    }
}
