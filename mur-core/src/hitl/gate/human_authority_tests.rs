//! Only the human settles a gate (#1764).
//!
//! The router key signs three kinds of `HitlResponse`: the human's answer
//! (`mur channel approve`, the phone — actor `Human`) and the gate's own
//! `--yes` / tier-grant audit record (actor `System`). `scan_prior` matches on
//! `action_hash` with a 7-day TTL, so if the audit record counted as a
//! decision, one `--yes` run would silently approve every later run of the
//! same action — including runs without `--yes`.

use super::*;
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

/// Nobody is watching: park and report blocked.
fn unattended() -> GatePolicy {
    GatePolicy {
        yes: false,
        unanswered: Unanswered::Defer,
        auto_approve_tiers: vec![],
    }
}

/// `--yes` at a terminal — the only mode in which it auto-approves.
fn interactive_yes() -> GatePolicy {
    GatePolicy {
        yes: true,
        unanswered: Unanswered::Wait,
        auto_approve_tiers: vec![],
    }
}

fn request_ids(home: &Path, ch: &str) -> Vec<String> {
    ChannelService::open(home)
        .unwrap()
        .load_events(ch)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .filter_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
        .map(|r| r.hitl_id)
        .collect()
}

/// Answer `hitl_id` with the router's key, speaking as `actor`.
fn answer_as(home: &Path, ch: &str, hitl_id: &str, actor: ChannelActor) {
    let svc = ChannelService::open(home).unwrap();
    let req: HitlRequest = svc
        .load_events(ch)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .filter_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
        .find(|r| r.hitl_id == hitl_id)
        .expect("request exists");
    let resp = HitlResponse {
        hitl_id: req.hitl_id,
        action_hash: req.action_hash,
        allow: true,
        reason: "test".into(),
        surface: "cli".into(),
    };
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        ch,
        ROUTER_AGENT,
        actor,
        EventKind::HitlResponse,
        serde_json::to_value(&resp).unwrap(),
        None,
    )
    .unwrap();
}

/// `--yes` is "Interactive/explicit only" (`GatePolicy::yes`). Its audit
/// record must not stand in for a human answer on the next run.
#[tokio::test]
async fn a_yes_audit_record_does_not_approve_a_later_run_without_yes() {
    let tmp = TempDir::new().unwrap();
    let _router = crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();

    let first = gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &interactive_yes(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        first.allow,
        "precondition: --yes approves this run: {first:?}"
    );

    let second = gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &unattended(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        !second.allow && second.deferred,
        "a later run without --yes must ask a human: {second:?}"
    );
    // It asks afresh rather than pointing at the --yes request, which every
    // listing already shows as answered — nobody would ever see it.
    let ids = request_ids(tmp.path(), &ch.id);
    assert_eq!(ids.len(), 2, "a new request is parked: {ids:?}");
    assert_eq!(second.hitl_id.as_deref(), ids.last().map(String::as_str));
}

/// The general form: a router-signed answer from the `System` actor is not
/// the human's, whoever holds the key.
#[tokio::test]
async fn a_router_signed_system_answer_does_not_release_the_gate() {
    let tmp = TempDir::new().unwrap();
    let _router = crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();

    let parked = gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &unattended(),
        None,
        None,
    )
    .await
    .unwrap();
    let id = parked.hitl_id.expect("parked");
    answer_as(tmp.path(), &ch.id, &id, ChannelActor::System);

    let next = gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &unattended(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!next.allow, "a System answer released the gate: {next:?}");
}

/// The control: the same answer from the human does release it.
#[tokio::test]
async fn a_router_signed_human_answer_still_releases_the_gate() {
    let tmp = TempDir::new().unwrap();
    let _router = crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();

    let parked = gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &unattended(),
        None,
        None,
    )
    .await
    .unwrap();
    let id = parked.hitl_id.expect("parked");
    answer_as(tmp.path(), &ch.id, &id, ChannelActor::local_human());

    let next = gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &unattended(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(next.allow, "the human's answer must release: {next:?}");
}
