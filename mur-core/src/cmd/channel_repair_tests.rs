//! #1965 acceptance: damaged → repair → the gate asks once and then reuses
//! the new answer; intact → no-op; the quarantine is kept and referenced;
//! `verify` is clean afterwards.

use super::*;
use crate::hitl::gate::{ActionRequest, GatePolicy, gate};
use mur_common::hitl::{HitlRequest, HitlResponse, RiskTier, Unanswered};
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

fn log_path(home: &Path, ch: &str) -> PathBuf {
    home.join("channels").join(ch).join("events.jsonl")
}

/// Answer `hitl_id` as the local human, signed and anchored by the router.
fn answer(home: &Path, ch: &str, hitl_id: &str, allow: bool) {
    try_answer(home, ch, hitl_id, allow).unwrap();
}

fn try_answer(home: &Path, ch: &str, hitl_id: &str, allow: bool) -> anyhow::Result<()> {
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
        allow,
        reason: "test".into(),
        surface: "cli".into(),
        issued_at: Some(chrono::Utc::now()),
        label: None,
    };
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        ch,
        ROUTER_AGENT,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(&resp).unwrap(),
        None,
    )
    .map(drop)
}

async fn ask(home: &Path, ch: &str) -> crate::hitl::gate::GateDecision {
    gate(home, ch, &destructive(), &unattended(), None, None)
        .await
        .unwrap()
}

/// A channel with an answered allow, then its tail cut off: damaged.
async fn damaged_channel(home: &Path) -> String {
    let _router = crate::channel_writer::plant_writer_identity(home);
    let ch = ChannelService::open(home)
        .unwrap()
        .create_for_workflow("g")
        .unwrap()
        .id;
    let id = ask(home, &ch).await.hitl_id.expect("parked");
    answer(home, &ch, &id, true);
    let text = std::fs::read_to_string(log_path(home, &ch)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    std::fs::write(
        log_path(home, &ch),
        lines[..lines.len() - 1].join("\n") + "\n",
    )
    .unwrap();
    assert!(!channel_integrity::check(home, &ch).unwrap().is_intact());
    ch
}

#[tokio::test]
async fn repair_lets_the_gate_ask_once_and_then_reuse_the_new_answer() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let ch = damaged_channel(home).await;

    // Before repair: an answer given on the damaged log is never reused.
    let stuck = ask(home, &ch).await;
    assert!(stuck.deferred, "{stuck:?}");
    // The line lands but the head cannot advance over a damaged log, so
    // the writer reports an anchor lag; the answer is on disk regardless.
    let lag = try_answer(home, &ch, stuck.hitl_id.as_deref().unwrap(), true).unwrap_err();
    assert!(
        lag.chain().any(|e| e
            .downcast_ref::<mur_channel::anchor::AnchorLagError>()
            .is_some()),
        "{lag:#}"
    );
    assert!(
        ask(home, &ch).await.deferred,
        "damaged log must keep asking"
    );

    let done = repair(home, &ch, "tester").unwrap().expect("was damaged");
    assert!(done.after.is_intact(), "{}", done.after.render());
    assert!(
        done.after.anchored_lines.is_some(),
        "fresh chain is anchored"
    );
    assert_eq!(done.repair_seq, 0, "repair record opens the fresh chain");

    // No decision crossed the repair: the gate asks once...
    let fresh = ask(home, &ch).await;
    assert!(fresh.deferred && !fresh.allow, "{fresh:?}");
    answer(home, &ch, fresh.hitl_id.as_deref().unwrap(), true);
    // ...and the new answer is reused from then on.
    let reused = ask(home, &ch).await;
    assert!(reused.allow && !reused.deferred, "{reused:?}");
    assert!(channel_integrity::check(home, &ch).unwrap().is_intact());
}

#[tokio::test]
async fn quarantine_is_kept_and_referenced_by_the_signed_repair_record() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let ch = damaged_channel(home).await;
    let old_log = std::fs::read(log_path(home, &ch)).unwrap();

    let done = repair(home, &ch, "tester").unwrap().unwrap();
    assert_eq!(
        std::fs::read(done.quarantine.join("events.jsonl")).unwrap(),
        old_log,
        "the damaged log is preserved byte for byte"
    );
    assert!(
        done.quarantine
            .join(mur_channel::store::QUARANTINED_HEAD_FILE)
            .is_file()
    );

    let events = ChannelService::open(home)
        .unwrap()
        .load_events(&ch)
        .unwrap();
    assert_eq!(events.len(), 1, "fresh chain holds only the repair record");
    let rec = &events[0];
    assert_eq!(rec.kind, EventKind::Note);
    let rel = done
        .quarantine
        .strip_prefix(home)
        .unwrap()
        .display()
        .to_string();
    assert_eq!(rec.payload["repair"]["quarantine"], rel.as_str());
    assert!(
        crate::channel_verify::verify_event(home, &ch, rec, true),
        "repair record must be router-signed"
    );
}

#[tokio::test]
async fn an_intact_channel_is_a_no_op() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let _router = crate::channel_writer::plant_writer_identity(home);
    let ch = ChannelService::open(home)
        .unwrap()
        .create_for_workflow("g")
        .unwrap()
        .id;
    ask(home, &ch).await;
    let log = std::fs::read(log_path(home, &ch)).unwrap();

    assert!(repair(home, &ch, "tester").unwrap().is_none());
    assert_eq!(
        run(home, &ch, true).unwrap(),
        channel_integrity::EXIT_INTACT
    );
    assert_eq!(std::fs::read(log_path(home, &ch)).unwrap(), log);
    assert!(!home.join("channels").join(&ch).join("quarantine").exists());
}

#[tokio::test]
async fn chat_gate_heads_are_resealed_over_the_fresh_log() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let ch = damaged_channel(home).await;
    let agent = crate::channel_writer::plant_identity_for(home, "helper");
    let heads = home.join("agents/helper").join(HITL_HEAD_DIR);
    std::fs::create_dir_all(&heads).unwrap();
    std::fs::write(heads.join(format!("{ch}.head")), b"stale").unwrap();

    let done = repair(home, &ch, "tester").unwrap().unwrap();
    assert_eq!(
        std::fs::read(done.quarantine.join("hitl-heads-helper.head")).unwrap(),
        b"stale"
    );
    let head = mur_channel::head_io::read_head(&heads, &ch)
        .unwrap()
        .unwrap();
    assert!(head.verify(&agent.verifying_key_bytes()));
    let log = std::fs::read(log_path(home, &ch)).unwrap();
    assert_eq!(head.lines, mur_channel::head::chain_log(&log).lines);
}

#[test]
fn repair_refuses_without_a_router_key_and_changes_nothing() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("g").unwrap().id;
    svc.append(
        &ch,
        ChannelActor::System,
        EventKind::Note,
        serde_json::json!({}),
        None,
    )
    .unwrap();
    drop(svc);
    std::fs::OpenOptions::new()
        .append(true)
        .open(log_path(home, &ch))
        .unwrap()
        .write_all(b"not json\n")
        .unwrap();
    let before = std::fs::read(log_path(home, &ch)).unwrap();

    let err = repair(home, &ch, "tester").unwrap_err();
    assert!(format!("{err:#}").contains("router key"), "{err:#}");
    assert_eq!(std::fs::read(log_path(home, &ch)).unwrap(), before);
}
