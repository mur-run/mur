//! C3 key matrix, through the gate's scan (`gate()` → `scan_prior`), not just
//! the predicate. Each case parks a request, writes one answer, and asks the
//! gate again: `allow` means the answer settled it.

use super::*;
use crate::hitl::authority_fixtures::{
    Labelled, enroll, human_signer, human_write, owned_workflow, previous_human_key, rekey_human,
};
use mur_channel::service::HumanSign;
use mur_common::channel::ChannelActor;
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

fn destructive() -> ActionRequest {
    ActionRequest {
        tier: RiskTier::Destructive,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({ "cmd": "rm -rf build" }),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        summary: "rm".into(),
    }
}

fn unattended() -> GatePolicy {
    GatePolicy {
        yes: false,
        unanswered: Unanswered::Defer,
        auto_approve_tiers: vec![],
    }
}

/// `--yes` at a terminal: the gate writes its own `System` audit answer.
fn interactive_yes() -> GatePolicy {
    GatePolicy {
        yes: true,
        unanswered: Unanswered::Wait,
        auto_approve_tiers: vec![],
    }
}

async fn ask(home: &Path, ch: &str) -> GateDecision {
    gate(home, ch, &destructive(), &unattended(), None, None)
        .await
        .unwrap()
}

/// Park a request and return the payload an allow for it carries.
async fn park(home: &Path, ch: &str) -> serde_json::Value {
    let d = ask(home, ch).await;
    assert!(d.deferred, "precondition: parked: {d:?}");
    let resp = HitlResponse {
        hitl_id: d.hitl_id.expect("parked id"),
        action_hash: d.action_hash,
        allow: true,
        reason: "test".into(),
        surface: "cli".into(),
        issued_at: Some(chrono::Utc::now()),
        label: None,
    };
    serde_json::to_value(resp).unwrap()
}

/// A home with the router key and an enrolled human.
fn home() -> TempDir {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    tmp
}

fn router_answer(home: &Path, ch: &str, actor: ChannelActor, payload: serde_json::Value) {
    let svc = ChannelService::open(home).unwrap();
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        ch,
        crate::channel_writer::Authorship::out_of_role(
            crate::channel_writer::Signer::ControlPlane,
            actor,
        ),
        EventKind::HitlResponse,
        payload,
        None,
    )
    .unwrap();
}

/// N4: the human's own key approves a deferred request, and the next run of
/// the gate is released. Before C3 the scan dropped every answer the router
/// did not sign, so this approval never reached the authority check.
#[tokio::test]
async fn deferred_human_key_approval_is_released_on_next_run() {
    let tmp = home();
    let ch = owned_workflow(tmp.path());
    let answer = park(tmp.path(), &ch).await;
    human_write(
        tmp.path(),
        &ch,
        &human_signer(tmp.path()),
        EventKind::HitlResponse,
        answer,
    );
    let next = ask(tmp.path(), &ch).await;
    assert!(next.allow, "a human-key approval must release: {next:?}");
}

/// Keyless, router-signed `Human` written after the record: past the cut
/// it is what the router writes for anyone who asks, so it is not authority.
#[tokio::test]
async fn router_signed_human_past_cut_is_not_authority() {
    let tmp = home();
    let ch = owned_workflow(tmp.path());
    let answer = park(tmp.path(), &ch).await;
    router_answer(tmp.path(), &ch, ChannelActor::local_human(), answer);
    let next = ask(tmp.path(), &ch).await;
    assert!(!next.allow, "keyless human past the cut released: {next:?}");
}

/// The same router-signed answer, written before the channel was enrolled
/// (so `seq <= human_cut`), still decides.
#[tokio::test]
async fn router_signed_human_before_cut_is_authority() {
    let tmp = home();
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g", mur_channel::service::ChannelOwner::Legacy)
        .unwrap()
        .id;
    let answer = park(tmp.path(), &ch).await;
    router_answer(tmp.path(), &ch, ChannelActor::local_human(), answer);
    enroll(tmp.path(), &ch);
    let next = ask(tmp.path(), &ch).await;
    assert!(
        next.allow,
        "a pre-cut human answer must still decide: {next:?}"
    );
}

/// An agent signing a `Human` claim with its own key is not the human.
#[tokio::test]
async fn agent_signed_human_is_not_authority() {
    let tmp = home();
    let qa = crate::channel_writer::plant_identity_for(tmp.path(), "qa");
    let ch = owned_workflow(tmp.path());
    let answer = park(tmp.path(), &ch).await;
    let key = mur_common::identity::encode_pubkey_bytes(&human_signer(tmp.path()).pubkey());
    ChannelService::open(tmp.path())
        .unwrap()
        .append_signed(
            &ch,
            &qa,
            0,
            ChannelActor::local_human_with_key(key),
            EventKind::HitlResponse,
            answer,
            None,
        )
        .unwrap();
    let next = ask(tmp.path(), &ch).await;
    assert!(
        !next.allow,
        "an agent-signed human claim released: {next:?}"
    );
}

/// Unsigned, claiming the human — what a sandboxed `mur channel approve`
/// writes.
#[tokio::test]
async fn unsigned_human_is_not_authority() {
    let tmp = home();
    let ch = owned_workflow(tmp.path());
    let answer = park(tmp.path(), &ch).await;
    ChannelService::open(tmp.path())
        .unwrap()
        .append(
            &ch,
            ChannelActor::local_human(),
            EventKind::HitlResponse,
            answer,
            None,
        )
        .unwrap();
    let next = ask(tmp.path(), &ch).await;
    assert!(!next.allow, "an unsigned human claim released: {next:?}");
}

/// `--yes` writes a router-signed `System` answer: audit, not consent, on
/// an owned channel as on a legacy one (#1764).
#[tokio::test]
async fn system_yes_is_not_authority_on_owned_channel() {
    let tmp = home();
    let ch = owned_workflow(tmp.path());
    let first = gate(
        tmp.path(),
        &ch,
        &destructive(),
        &interactive_yes(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(first.allow, "precondition: --yes approves its own run");
    let next = ask(tmp.path(), &ch).await;
    assert!(!next.allow && next.deferred, "--yes carried over: {next:?}");
}

/// D-9: an approval signed by the human key in force before a rekey is a
/// trusted human event for display, but grants nothing after the rekey.
#[tokio::test]
async fn pre_rekey_human_approval_is_not_authority_after_rekey() {
    let tmp = home();
    let ch = owned_workflow(tmp.path());
    let answer = park(tmp.path(), &ch).await;
    human_write(
        tmp.path(),
        &ch,
        &human_signer(tmp.path()),
        EventKind::HitlResponse,
        answer,
    );
    rekey_human(tmp.path());
    let next = ask(tmp.path(), &ch).await;
    assert!(!next.allow, "a pre-rekey approval released: {next:?}");
}

/// D-9 rev 4: the old key's signature carrying the head's `key_version` is
/// still the old key. Authority verifies under the head key and never picks
/// a key by the label.
#[tokio::test]
async fn old_key_labelled_with_head_version_is_not_authority() {
    let tmp = home();
    let ch = owned_workflow(tmp.path());
    rekey_human(tmp.path());
    let answer = park(tmp.path(), &ch).await;
    let old = Labelled {
        key: previous_human_key(tmp.path()),
        key_version: human_signer(tmp.path()).key_version(),
    };
    human_write(tmp.path(), &ch, &old, EventKind::HitlResponse, answer);
    let next = ask(tmp.path(), &ch).await;
    assert!(
        !next.allow,
        "an old key under the head label released: {next:?}"
    );
}

/// D-7: a record naming somebody else's key is ignored, so the channel stays
/// unowned for that stranger and the local human enrolls and approves.
#[tokio::test]
async fn foreign_record_channel_local_human_still_approves() {
    let tmp = home();
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g", mur_channel::service::ChannelOwner::Legacy)
        .unwrap()
        .id;
    // Another machine's human, signing its own record with its own key.
    let stranger = AgentIdentity::generate();
    let key = stranger.pubkey_text();
    let payload = serde_json::to_value(mur_common::channel::OwnerKeyEnrolledPayload {
        owner_key: key.clone(),
        key_version: 0,
        human_cut: None,
    })
    .unwrap();
    ChannelService::open(tmp.path())
        .unwrap()
        .append_signed(
            &ch,
            &stranger,
            0,
            ChannelActor::local_human_with_key(key),
            EventKind::OwnerKeyEnrolled,
            payload,
            None,
        )
        .unwrap();
    let answer = park(tmp.path(), &ch).await;
    human_write(
        tmp.path(),
        &ch,
        &human_signer(tmp.path()),
        EventKind::HitlResponse,
        answer,
    );
    let next = ask(tmp.path(), &ch).await;
    assert!(next.allow, "the local human must still approve: {next:?}");
}
