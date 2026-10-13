//! The approval fold: only the human, answering the router's own request for
//! exactly this pending row, releases a push (plan T12, spec §4, #1764).
#![cfg(unix)]

use chrono::{DateTime, Duration, Utc};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::hitl::{HitlRequest, HitlResponse, RiskTier};
use mur_common::identity::AgentIdentity;
use mur_daemon::git_push_broker::{Folded, HumanApprovalVerifier, fold_approvals};
use mur_git_broker::pending::RequestKey;
use serde_json::json;
use std::path::Path;

/// The router's on-disk agent slug (`mur_core::channel_writer::ROUTER_AGENT`).
const ROUTER: &str = mur_core::channel_writer::ROUTER_AGENT;
/// Opaque to the fold; any fixed string.
const H: &str = "aa";

struct World {
    home: tempfile::TempDir,
    svc: ChannelService,
    ch: String,
    router: AgentIdentity,
    agent: AgentIdentity,
}

fn plant(home: &Path, who: &str) -> AgentIdentity {
    let d = home.join("agents").join(who);
    std::fs::create_dir_all(&d).unwrap();
    let id = AgentIdentity::generate();
    id.save(&d).unwrap();
    id
}

fn world() -> World {
    let home = tempfile::tempdir().unwrap();
    let router = plant(home.path(), ROUTER);
    let agent = plant(home.path(), "bob");
    let svc = ChannelService::open(home.path()).unwrap();
    let ch = svc.create_for_agent("bob").unwrap().id;
    World {
        home,
        svc,
        ch,
        router,
        agent,
    }
}

fn key(id: &str) -> RequestKey {
    RequestKey {
        agent_id: "bob".into(),
        task_id: "t1".into(),
        request_id: id.into(),
    }
}

fn req_payload(id: &str, hash: &str, at: DateTime<Utc>) -> serde_json::Value {
    serde_json::to_value(HitlRequest {
        hitl_id: id.into(),
        action_hash: hash.into(),
        tier: RiskTier::Destructive,
        tool_name: "git_push".into(),
        tool_input: json!({}),
        step_or_call_id: id.into(),
        agent_id: "bob".into(),
        timeout_ms: 0,
        summary: "push".into(),
        issued_at: Some(at),
    })
    .unwrap()
}

fn resp_payload(id: &str, hash: &str, allow: bool, at: DateTime<Utc>) -> serde_json::Value {
    serde_json::to_value(HitlResponse {
        hitl_id: id.into(),
        action_hash: hash.into(),
        allow,
        reason: String::new(),
        surface: "hub".into(),
        issued_at: Some(at),
        label: None,
    })
    .unwrap()
}

/// Router-signed request, as the daemon writes it.
fn ask(w: &World, id: &str, hash: &str) {
    w.svc
        .append_signed(
            &w.ch,
            &w.router,
            0,
            ChannelActor::System,
            EventKind::HitlRequest,
            req_payload(id, hash, Utc::now()),
            None,
        )
        .unwrap();
}

fn answer_at(
    w: &World,
    who: &AgentIdentity,
    actor: ChannelActor,
    id: &str,
    hash: &str,
    allow: bool,
    at: DateTime<Utc>,
) {
    w.svc
        .append_signed(
            &w.ch,
            who,
            0,
            actor,
            EventKind::HitlResponse,
            resp_payload(id, hash, allow, at),
            None,
        )
        .unwrap();
}

fn answer_as(
    w: &World,
    who: &AgentIdentity,
    actor: ChannelActor,
    id: &str,
    hash: &str,
    allow: bool,
) {
    answer_at(w, who, actor, id, hash, allow, Utc::now());
}

/// The human's answer: router key, `Human` actor (`mur channel approve`, the phone).
fn human_says(w: &World, id: &str, hash: &str, allow: bool) {
    answer_as(w, &w.router, human(), id, hash, allow);
}

fn human() -> ChannelActor {
    ChannelActor::Human {
        name: "human".into(),
    }
}

fn fold(w: &World, pending: &[(&str, &str)]) -> Vec<Folded> {
    let ev = w.svc.load_events(&w.ch).unwrap();
    let p: Vec<_> = pending
        .iter()
        .map(|(a, b)| (key(a), b.to_string()))
        .collect();
    fold_approvals(w.home.path(), &w.ch, &ev, &p)
}

#[test]
fn human_allow_for_a_router_request_with_the_same_hash_advances() {
    let w = world();
    ask(&w, "r1", H);
    human_says(&w, "r1", H, true);
    let f = fold(&w, &[("r1", H)]);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].proof.request_id, "r1");
    assert_eq!(f[0].proof.action_hash, H);
    assert_eq!(f[0].key, key("r1"));
}

#[test]
fn agent_signed_allow_does_not_advance() {
    let w = world();
    ask(&w, "r1", H);
    answer_as(
        &w,
        &w.agent,
        ChannelActor::Agent { id: "bob".into() },
        "r1",
        H,
        true,
    );
    assert!(fold(&w, &[("r1", H)]).is_empty());
}

#[test]
fn agent_key_speaking_as_human_does_not_advance() {
    let w = world();
    ask(&w, "r1", H);
    answer_as(&w, &w.agent, human(), "r1", H, true);
    assert!(
        fold(&w, &[("r1", H)]).is_empty(),
        "a Human actor must carry the router's signature"
    );
}

#[test]
fn router_signed_system_response_does_not_advance() {
    // F22/F23 and #1764: the router's own audit record is not a human decision.
    let w = world();
    ask(&w, "r1", H);
    answer_as(&w, &w.router, ChannelActor::System, "r1", H, true);
    assert!(fold(&w, &[("r1", H)]).is_empty());
}

#[test]
fn deny_never_produces_a_proof() {
    let w = world();
    ask(&w, "r1", H);
    human_says(&w, "r1", H, false);
    assert!(fold(&w, &[("r1", H)]).is_empty());
}

#[test]
fn a_later_deny_overrides_an_earlier_allow() {
    let w = world();
    ask(&w, "r1", H);
    let t = Utc::now();
    answer_at(&w, &w.router, human(), "r1", H, true, t);
    answer_at(
        &w,
        &w.router,
        human(),
        "r1",
        H,
        false,
        t + Duration::seconds(1),
    );
    assert!(fold(&w, &[("r1", H)]).is_empty());
}

#[test]
fn response_hash_must_equal_the_request_hash() {
    let w = world();
    ask(&w, "r1", H);
    human_says(&w, "r1", "bb", true);
    assert!(fold(&w, &[("r1", H)]).is_empty());
    assert!(
        fold(&w, &[("r1", "bb")]).is_empty(),
        "nor may the row's hash differ from the request's"
    );
}

#[test]
fn agent_written_request_is_never_answerable() {
    let w = world();
    w.svc
        .append_signed(
            &w.ch,
            &w.agent,
            0,
            ChannelActor::Agent { id: "bob".into() },
            EventKind::HitlRequest,
            req_payload("r1", H, Utc::now()),
            None,
        )
        .unwrap();
    human_says(&w, "r1", H, true);
    assert!(fold(&w, &[("r1", H)]).is_empty());
}

#[test]
fn hitl_id_matching_no_router_request_does_not_advance() {
    let w = world();
    human_says(&w, "ghost", H, true);
    assert!(fold(&w, &[("ghost", H)]).is_empty());
}

#[test]
fn reissued_request_id_answers_nothing() {
    let w = world();
    ask(&w, "r1", H);
    ask(&w, "r1", H);
    human_says(&w, "r1", H, true);
    assert!(fold(&w, &[("r1", H)]).is_empty());
}

#[test]
fn different_channel_id_does_not_advance() {
    let w = world();
    ask(&w, "r1", H);
    human_says(&w, "r1", H, true);
    let ev = w.svc.load_events(&w.ch).unwrap();
    assert!(
        fold_approvals(
            w.home.path(),
            "some-other-channel",
            &ev,
            &[(key("r1"), H.into())]
        )
        .is_empty(),
        "signatures bind the channel id"
    );
}

#[test]
fn unsigned_human_looking_allow_is_refused() {
    // F10: a token-less shim writes unsigned events.
    let w = world();
    ask(&w, "r1", H);
    w.svc
        .append(
            &w.ch,
            human(),
            EventKind::HitlResponse,
            resp_payload("r1", H, true, Utc::now()),
            None,
        )
        .unwrap();
    assert!(fold(&w, &[("r1", H)]).is_empty());
}

#[test]
fn answer_after_the_ttl_does_not_advance() {
    let w = world();
    ask(&w, "r1", H);
    let late = Utc::now() + Duration::seconds(mur_common::hitl::APPROVAL_TTL_SECS * 2);
    answer_at(&w, &w.router, human(), "r1", H, true, late);
    assert!(fold(&w, &[("r1", H)]).is_empty());
}

#[test]
fn nothing_pending_releases_nothing() {
    // A valid old allow in the log is not consulted once its row is gone.
    let w = world();
    ask(&w, "r1", H);
    human_says(&w, "r1", H, true);
    assert!(fold(&w, &[]).is_empty());
}

#[test]
fn one_event_yields_one_stable_proof_id() {
    let w = world();
    ask(&w, "r1", H);
    human_says(&w, "r1", H, true);
    let a = fold(&w, &[("r1", H)]);
    let b = fold(&w, &[("r1", H)]);
    assert_eq!(
        a[0].proof.event_id, b[0].proof.event_id,
        "the broker tombstone makes a replay inert"
    );
}

#[test]
fn verifier_adapter_folds_its_own_channel() {
    let w = world();
    ask(&w, "r1", H);
    human_says(&w, "r1", H, true);
    let ev = w.svc.load_events(&w.ch).unwrap();
    let v = HumanApprovalVerifier::new(w.home.path(), w.ch.clone());
    assert_eq!(v.verify(&ev, &[(key("r1"), H.into())]).len(), 1);
    let other = HumanApprovalVerifier::new(w.home.path(), "other");
    assert!(other.verify(&ev, &[(key("r1"), H.into())]).is_empty());
}

#[test]
fn adapter_calls_the_shared_predicate_and_never_copies_it() {
    let src = include_str!("../src/git_push_broker.rs");
    assert!(src.contains("is_human_authority(mur_home, channel_id, e)"));
    assert!(
        src.contains("Requests::collect"),
        "binds through the gate's own rule"
    );
    // The parent module and every submodule: a copy must not hide in one.
    let all = [src, include_str!("../src/git_push_broker/intake.rs")];
    for local in [
        "fn is_human_authority",
        "fn is_router_signed",
        "fn bind(",
        "verify_one(",
    ] {
        for file in all {
            assert!(!file.contains(local), "no local copy: {local} (spec §4)");
        }
    }
}

#[test]
fn row_hash_must_equal_the_approved_hash() {
    // The human approved H; the pending row is for another action under the same id.
    let w = world();
    ask(&w, "r1", H);
    human_says(&w, "r1", H, true);
    assert!(fold(&w, &[("r1", "bb")]).is_empty());
}

#[test]
fn deny_wins_a_tie_on_signed_time() {
    for deny_first in [true, false] {
        let w = world();
        ask(&w, "r1", H);
        let t = Utc::now();
        for allow in if deny_first {
            [false, true]
        } else {
            [true, false]
        } {
            answer_at(&w, &w.router, human(), "r1", H, allow, t);
        }
        assert!(fold(&w, &[("r1", H)]).is_empty(), "deny_first={deny_first}");
    }
}

#[test]
fn proof_id_ignores_unsigned_store_fields() {
    // `seq` / `ts` are store-assigned and unsigned: a copied line is the same approval.
    let w = world();
    ask(&w, "r1", H);
    human_says(&w, "r1", H, true);
    let ev = w.svc.load_events(&w.ch).unwrap();
    let mut moved = ev.clone();
    let last = moved.last_mut().unwrap();
    last.seq += 1000;
    last.ts += Duration::seconds(5);
    let p = [(key("r1"), H.to_string())];
    let a = fold_approvals(w.home.path(), &w.ch, &ev, &p);
    let b = fold_approvals(w.home.path(), &w.ch, &moved, &p);
    assert_eq!(a[0].proof.event_id, b[0].proof.event_id);
}
