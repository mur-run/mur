//! Per-actor verify-on-fold (v3d-2): each event is verified against ITS actor's
//! pubkey (`<mur_home>/agents/<id>`), not a single channel writer.
use std::path::Path;

use mur_common::channel::{ChannelActor, ChannelEvent};

/// Resolve the pubkey that should have signed `actor`'s events. Agent{id} →
/// that agent's home; System/Human → the router ("mur") which writes them.
pub fn actor_pubkey(
    mur_home: &Path,
    actor: &ChannelActor,
    key_version: Option<u32>,
) -> Option<[u8; 32]> {
    let agent = match actor {
        ChannelActor::Agent { id } => id.as_str(),
        _ => crate::channel_writer::ROUTER_AGENT,
    };
    mur_channel::sign::resolve_writer_pubkey(&mur_home.join("agents").join(agent), key_version)
}

/// True if `ev` verifies against its actor's key (present sig must verify;
/// missing sig tolerated iff `!require_sig`).
///
/// An **unresolvable key is not the same as an absent signature.** Tolerating
/// a missing signature is the migration-safety rule for legacy unsigned events
/// (v3d). An event that DOES carry a signature but whose key cannot be read has
/// not been verified by anything — accepting it would let an unreadable key
/// directory switch verification off silently, which is precisely what a
/// deny-by-default sandbox produces on Linux (`agents/` is granted per-file, so
/// a peer created after the seal is unreadable). It fails closed instead.
pub fn verify_event(
    mur_home: &Path,
    channel_id: &str,
    ev: &ChannelEvent,
    require_sig: bool,
) -> bool {
    match actor_pubkey(mur_home, &ev.actor, ev.key_version) {
        Some(pk) => mur_channel::sign::verify_one(channel_id, ev, &pk, require_sig),
        // Unsigned: the legacy-tolerance rule still applies — there is nothing
        // a key would have checked.
        None if ev.sig.is_none() => !require_sig,
        None => {
            tracing::warn!(
                channel_id,
                actor = ?ev.actor,
                key_version = ?ev.key_version,
                "event carries a signature but its actor's public key could not                  be read — treating as UNVERIFIED (an unreadable key must not                  pass as an absent signature)"
            );
            false
        }
    }
}

/// Parse `MUR_CHANNEL_REQUIRE_SIG`: only explicit truthy values enable
/// signature enforcement (`=0` / `=false`, or unset, must NOT turn it on).
/// Shared by every reader of the var so the parsing rule lives in one place.
pub(crate) fn require_sig_from_env() -> bool {
    std::env::var("MUR_CHANNEL_REQUIRE_SIG")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::EventKind;
    use mur_common::identity::AgentIdentity;

    /// A SIGNED event whose actor key cannot be read must not pass, even with
    /// enforcement off. Otherwise an unreadable key directory — which is the
    /// normal state under a deny-by-default sandbox — silently turns signature
    /// verification into a no-op, and a forged event is indistinguishable from
    /// a genuine one.
    #[test]
    fn a_signed_event_with_an_unreadable_key_does_not_verify() {
        let tmp = tempfile::TempDir::new().unwrap();
        let qa_home = tmp.path().join("agents").join("qa");
        std::fs::create_dir_all(&qa_home).unwrap();
        let qa = AgentIdentity::generate();
        qa.save(&qa_home).unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
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
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
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

    #[test]
    fn event_verifies_against_its_own_actor_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let qa_home = tmp.path().join("agents").join("qa");
        std::fs::create_dir_all(&qa_home).unwrap();
        let qa = AgentIdentity::generate();
        qa.save(&qa_home).unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
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
            mur_channel::sign::sign_event(&imposter, &ch.id, &ev.actor, ev.kind, &ev.payload, None);
        let mut forged = ev.clone();
        forged.sig = Some(forged_sig);
        assert!(
            !verify_event(tmp.path(), &ch.id, &forged, true),
            "imposter sig rejected"
        );
    }
}
