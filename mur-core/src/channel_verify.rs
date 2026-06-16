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
pub fn verify_event(
    mur_home: &Path,
    channel_id: &str,
    ev: &ChannelEvent,
    require_sig: bool,
) -> bool {
    match actor_pubkey(mur_home, &ev.actor, ev.key_version) {
        Some(pk) => mur_channel::sign::verify_one(channel_id, ev, &pk, require_sig),
        None => !require_sig,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::EventKind;
    use mur_common::identity::AgentIdentity;

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
