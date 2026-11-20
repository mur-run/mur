//! D-10 (A1): a signed `run_paused` across a router rekey, through the same
//! two steps `cmd_fleet_resume` takes — `signed_pause` over the channel, then
//! `resume_from` over the (edited) checkpoint.

use super::super::pause_events::{PauseReason, RunBounds};
use super::*;
use crate::hitl::authority_fixtures::rekey_router;
use mur_common::channel::ChannelActor;
use mur_common::identity::{AgentIdentity, RotationReason};

const RUN: &str = "paused-run";
const SIGNED_COST: f64 = 5.0;
const SIGNED_ITERATION: u32 = 4;
const SIGNED_SPENT: f64 = 3.0;

fn pause_note() -> serde_json::Value {
    FleetRunPayload::RunPaused {
        run_id: RUN.into(),
        iteration_run_id: format!("{RUN}-{SIGNED_ITERATION}"),
        iteration: SIGNED_ITERATION,
        spent_usd: SIGNED_SPENT,
        active_elapsed_ms: 60_000,
        reason: PauseReason::Operator,
        bounds: Some(RunBounds {
            deadline_ms: 600_000,
            stuck_ms: Some(300_000),
            cost_usd: Some(SIGNED_COST),
        }),
    }
    .to_note_payload()
}

/// What an agent leaves after editing a paused run's checkpoint: the cost cap
/// gone and the counters lowered.
fn edited_checkpoint() -> Checkpoint {
    Checkpoint {
        schema: super::super::checkpoint::CHECKPOINT_SCHEMA,
        run_id: RUN.into(),
        iteration_run_id: format!("{RUN}-0"),
        fleet: "dev".into(),
        iteration: 0,
        spent_usd: 0.0,
        active_elapsed_ms: 0,
        stuck_elapsed_ms: 0,
        start_seq: 0,
        job_id: None,
        goal_override: None,
        deadline_ms: 600_000,
        stuck_ms: Some(300_000),
        cost_usd: None,
        hitl_unanswered: "defer".into(),
        target_cwd: None,
        procedure: vec![],
        paused_at: "2026-10-22T00:00:00Z".into(),
        binary_version: "test".into(),
    }
}

/// A fleet channel holding one `run_paused` signed by `signer` (labelled
/// `kv`) as `actor`, after the router `old` was rotated with `reason`.
fn resume_after(
    reason: Option<RotationReason>,
    signer: impl Fn(&AgentIdentity) -> (AgentIdentity, u32, ChannelActor),
) -> ResumeFrom {
    let tmp = tempfile::tempdir().unwrap();
    let old = crate::channel_writer::plant_writer_identity(tmp.path());
    crate::channel_writer::plant_identity_for(tmp.path(), "pm");
    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    let ch = svc
        .create_for_workflow("dev", mur_channel::service::ChannelOwner::Legacy)
        .unwrap()
        .id;
    let (id, kv, actor) = signer(&old);
    svc.append_signed(&ch, &id, kv, actor, EventKind::Note, pause_note(), None)
        .unwrap();
    if let Some(r) = reason {
        rekey_router(tmp.path(), &old, r);
    }
    let events = svc.load_events(&ch).unwrap();
    let signed = signed_pause(tmp.path(), &ch, &events, RUN).unwrap();
    resume_from(&edited_checkpoint(), signed, true).unwrap()
}

fn router_pause(old: &AgentIdentity) -> (AgentIdentity, u32, ChannelActor) {
    (clone(old), 0, ChannelActor::System)
}

fn clone(id: &AgentIdentity) -> AgentIdentity {
    AgentIdentity::from_secret_bytes(&id.signing_key().to_bytes())
}

fn keeps_signed(r: &ResumeFrom) {
    assert!(r.bounds_authenticated, "signed pause was dropped: {r:?}");
    assert_eq!(r.cost_usd, Some(SIGNED_COST), "cost cap lost: {r:?}");
    assert_eq!(r.iteration, SIGNED_ITERATION);
    assert_eq!(r.spent_usd, SIGNED_SPENT);
}

/// A routine rekey does not revoke the signed pause: the edited checkpoint
/// cannot drop the cost cap or lower the counters.
#[test]
fn routine_rekey_keeps_signed_bounds_and_counters() {
    keeps_signed(&resume_after(Some(RotationReason::Scheduled), router_pause));
    keeps_signed(&resume_after(
        Some(RotationReason::OwnerChange),
        router_pause,
    ));
}

/// A compromise rekey revokes it: resume falls back to the checkpoint and
/// says the bounds are unauthenticated (today's warning path).
#[test]
fn compromise_rekey_rejects_the_signed_pause() {
    let r = resume_after(Some(RotationReason::SuspectCompromise), router_pause);
    assert!(!r.bounds_authenticated, "{r:?}");
    assert_eq!(r.cost_usd, None, "the checkpoint is all there is");
}

/// The old key's signature labelled with the head's `key_version` still
/// verifies — every routine chain key is tried, the label picks nothing.
#[test]
fn old_key_labelled_as_head_is_verified_by_trying_chain_keys() {
    let r = resume_after(Some(RotationReason::Scheduled), |old| {
        (clone(old), 1, ChannelActor::System)
    });
    keeps_signed(&r);
}

/// A member forging a `run_paused` as `System` with its own key verifies
/// under no chain key, before or after a rekey.
#[test]
fn agent_signed_forged_pause_is_rejected_under_every_chain_key() {
    for reason in [None, Some(RotationReason::Scheduled)] {
        let r = resume_after(reason, |_| {
            (AgentIdentity::generate(), 0, ChannelActor::System)
        });
        assert!(!r.bounds_authenticated, "{reason:?}: {r:?}");
    }
}
