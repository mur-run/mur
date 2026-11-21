//! #2081 item 7: the writer and the verifier name the signer through one
//! type, [`Signer`]. These pin the two halves against each other.

use super::*;
use tempfile::TempDir;

const CUSTOM_ROUTER: &str = "orchestrator";

/// A home holding the control plane's key and a custom router's.
fn home_with_keys() -> (TempDir, ChannelService) {
    let tmp = TempDir::new().unwrap();
    plant_writer_identity(tmp.path());
    plant_identity_for(tmp.path(), CUSTOM_ROUTER);
    let svc = ChannelService::open(tmp.path()).unwrap();
    (tmp, svc)
}

/// One channel per signer. These tests are about an event's own signature;
/// two disk-key signers on one unowned channel also contend for its signed
/// head, which is a separate matter (#2081 audit) and would fail the write
/// here before the signature is ever looked at.
fn channel(svc: &ChannelService, label: &str) -> String {
    svc.create_for_workflow(label, mur_channel::service::ChannelOwner::Legacy)
        .unwrap()
        .id
}

fn write(
    svc: &ChannelService,
    home: &Path,
    channel: &str,
    signer: Signer<'_>,
    actor: ChannelActor,
) -> ChannelEvent {
    append_as_writer(
        svc,
        home,
        channel,
        signer,
        actor,
        EventKind::Note,
        serde_json::json!({ "text": "x" }),
        None,
    )
    .unwrap()
}

/// Whatever the actor, an event written with `Signer::for_actor(&actor)` is
/// one the verifier accepts with signatures required. A writer that follows
/// the type cannot produce an event its own reader drops.
#[test]
fn an_event_written_by_its_actors_signer_verifies() {
    let (tmp, svc) = home_with_keys();
    let actors = [
        ChannelActor::System,
        ChannelActor::local_human(),
        ChannelActor::Agent {
            id: CUSTOM_ROUTER.into(),
        },
    ];
    for (i, actor) in actors.into_iter().enumerate() {
        let id = channel(&svc, &format!("actor-{i}"));
        let signer = Signer::for_actor(&actor);
        let ev = write(&svc, tmp.path(), &id, signer, actor.clone());
        assert!(ev.sig.is_some(), "{actor:?} must be signed");
        assert!(
            crate::channel_verify::verify_event(tmp.path(), &id, &ev, true),
            "{actor:?} written by its own signer must verify"
        );
    }
}

/// The control plane is the only signer `hitl::authority` takes a `System`
/// event from, and it is not the fleet's configured router. A custom router
/// signing as `System` writes an event nothing authenticates — the mismatch
/// behind #2072 and #2116, and the reason the verifier must never be
/// "fixed" to trust `fleet.router`.
#[test]
fn a_system_event_under_a_custom_routers_key_authenticates_nowhere() {
    use crate::hitl::authority::{AuthorityCtx, is_control_plane_signed, is_durable_router_record};
    let (tmp, svc) = home_with_keys();
    let home = tmp.path();

    let id = channel(&svc, "control-plane");
    let ours = write(&svc, home, &id, Signer::ControlPlane, ChannelActor::System);
    let ctx = AuthorityCtx::load(home, &id).unwrap();
    assert!(crate::channel_verify::verify_event(home, &id, &ours, true));
    assert!(is_control_plane_signed(&ctx, &ours));
    assert!(is_durable_router_record(&ctx, &ours));

    let id = channel(&svc, "custom-router");
    let signer = Signer::Agent(CUSTOM_ROUTER);
    let theirs = write(&svc, home, &id, signer, ChannelActor::System);
    let ctx = AuthorityCtx::load(home, &id).unwrap();
    assert!(theirs.sig.is_some(), "it is signed — by the wrong identity");
    assert!(!crate::channel_verify::verify_event(
        home, &id, &theirs, true
    ));
    assert!(!is_control_plane_signed(&ctx, &theirs));
    assert!(!is_durable_router_record(&ctx, &theirs));
}

/// A `HitlRequest` takes no signer: only the control plane can ask.
#[test]
fn a_hitl_request_is_always_signed_by_the_control_plane() {
    let (tmp, svc) = home_with_keys();
    let id = channel(&svc, "request");
    let ev = append_hitl_request(&svc, tmp.path(), &id, serde_json::json!({}), true).unwrap();
    assert_eq!(ev.actor, ChannelActor::System);
    let ctx = crate::hitl::authority::AuthorityCtx::load(tmp.path(), &id).unwrap();
    assert!(crate::hitl::authority::is_control_plane_signed(&ctx, &ev));
}
