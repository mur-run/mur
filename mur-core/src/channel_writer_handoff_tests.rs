//! #2081 items 5 and 6: what a sealed fleet loop can sign.
//!
//! A loop the `fleet_run` tool starts inside an agent's sandbox is handed
//! one key, its trigger agent's, and `keys/` is denied to it. The handoff is
//! honoured for that agent's name only, so every `Signer::ControlPlane`
//! write such a loop makes — `run_paused`, `run_resumed`, the stop
//! `StateChange`, a HITL request — has no key unless the trigger agent IS
//! the control plane.
//!
//! These pin that as the writer's behaviour. It is why a fleet routed by its
//! own non-control-plane trigger agent is launched by the daemon instead
//! (`fleet_run::gate::runs_locally`): the writer is right to refuse, so the
//! run must not be started where it would have to.

use super::*;
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

/// A trigger agent that routes its own fleet and is not the control plane.
const SELF_ROUTER: &str = "orchestrator";

fn handoff() -> Handoff {
    Handoff {
        agent: SELF_ROUTER.into(),
        kv: 1,
        id: AgentIdentity::generate(),
    }
}

fn refuse(require_sig: bool, awaits_answer: bool) -> Refuse {
    Refuse {
        require_sig,
        awaits_answer,
    }
}

/// The name a `System` event is signed under, taken from the type the
/// verifier reads it back through rather than spelled here.
fn control_plane() -> String {
    Authorship::system().signer().agent_name().to_string()
}

/// A home as the sealed loop sees it: the control plane has a key, and this
/// process may not read it. Restores the mode so the `TempDir` can go.
#[cfg(unix)]
fn with_sealed_keys<T>(f: impl FnOnce(&Path, &ChannelService, &str) -> T) -> T {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    plant_writer_identity(home);
    let svc = ChannelService::open(home).unwrap();
    let channel = svc
        .create_for_workflow("fleet", mur_channel::service::ChannelOwner::Legacy)
        .unwrap()
        .id;
    let key = mur_common::identity::private_key_dir(&home.join("agents").join(CONTROL_PLANE_AGENT))
        .join("identity.key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
    let out = f(home, &svc, &channel);
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    out
}

fn append_system(
    handoff: &Handoff,
    svc: &ChannelService,
    home: &Path,
    channel: &str,
    require_sig: bool,
) -> anyhow::Result<ChannelEvent> {
    append_as_writer_with(
        Some(handoff),
        svc,
        home,
        channel,
        &control_plane(),
        Authorship::system().into_actor(),
        EventKind::Note,
        serde_json::json!({ "kind": "run_paused" }),
        None,
        require_sig,
    )
}

/// The control: the handoff does sign, for the agent it was issued to. If
/// this fails the cases below prove nothing about the control plane.
#[test]
fn a_handoff_signs_as_the_agent_it_was_issued_to() {
    let tmp = TempDir::new().unwrap();
    let handoff = handoff();
    let key = resolve_writer_key_in(Some(&handoff), tmp.path(), SELF_ROUTER, refuse(true, false))
        .expect("the handed-over key must not be refused")
        .expect("the handed-over key must be returned");
    assert_eq!(key.source, KeySource::Handoff);
    assert_eq!(
        key.id.verifying_key_bytes(),
        handoff.id.verifying_key_bytes()
    );
}

/// With signatures optional, a control-plane event from a loop holding only
/// its router's key is written UNSIGNED — never signed with the router's
/// key, which `hitl::authority` would not take a `System` event from.
#[cfg(unix)]
#[test]
fn a_control_plane_append_under_another_agents_handoff_is_unsigned() {
    let handoff = handoff();
    let ev = with_sealed_keys(|home, svc, channel| {
        append_system(&handoff, svc, home, channel, false)
            .expect("the optional path writes rather than refuses")
    });
    assert!(
        ev.sig.is_none(),
        "a control-plane event must not be signed with '{SELF_ROUTER}'s handed-over key"
    );
}

/// With signatures required there is nothing to write: the reader would drop
/// it. The refusal names the control plane, the signer that is missing.
#[cfg(unix)]
#[test]
fn a_control_plane_append_under_another_agents_handoff_is_refused_when_signatures_are_required() {
    // Records a refusal: hold the lock the exact-count tests hold.
    let _guard = super::tests::COUNTER
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let handoff = handoff();
    let mark = refusal_count();
    let (err, written) = with_sealed_keys(|home, svc, channel| {
        let err = append_system(&handoff, svc, home, channel, true)
            .expect_err("a required signature with no usable key must refuse");
        (err, svc.load_events(channel).unwrap().len())
    });
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&format!("as '{}'", control_plane())) && msg.contains("unreadable"),
        "the refusal must name the missing signer and the cause: {msg}"
    );
    assert_eq!(written, 0, "a refused write leaves no event behind");
    assert_eq!(refusals_since(mark), 1, "the refusal must be counted");
}

/// A request a human must answer is refused whatever the enforcement flag
/// says (#2379): unsigned, it could never be approved.
#[cfg(unix)]
#[test]
fn a_request_awaiting_an_answer_under_another_agents_handoff_is_unanswerable() {
    let _guard = super::tests::COUNTER
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let handoff = handoff();
    let err = with_sealed_keys(|home, _, _| {
        match resolve_writer_key_in(Some(&handoff), home, &control_plane(), refuse(false, true)) {
            Err(e) => e,
            Ok(k) => panic!("must refuse, got Ok(is_some={})", k.is_some()),
        }
    });
    assert!(
        err.downcast_ref::<UnanswerableRequest>().is_some(),
        "typed, so the caller can name the surface that can ask: {err:#}"
    );
}
