//! #1965 acceptance: damaged → repair → the gate asks once and then reuses
//! the new answer; intact → no-op; the quarantine is kept and referenced;
//! `verify` is clean afterwards.

use super::*;
use crate::hitl::gate::{ActionRequest, GatePolicy, gate};
use mur_channel::ChannelStore;
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

    // Before repair: main's #1935 gate fails closed. A re-ask cannot be
    // sealed over the damaged log, so the action is denied outright and
    // stays denied however often it is retried.
    for _ in 0..2 {
        let stuck = ask(home, &ch).await;
        assert!(!stuck.allow && !stuck.deferred, "{stuck:?}");
        assert!(stuck.hitl_id.is_none(), "{stuck:?}");
        assert!(stuck.reason.contains("integrity check failed"), "{stuck:?}");
    }

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

/// Run `f` with the append lock held from another handle, as a writer
/// mid-append would.
fn hold_lock_while<T>(home: &Path, ch: &str, f: impl FnOnce() -> T) -> T {
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(home.join("channels").join(ch).join("events.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let out = f();
    FileExt::unlock(&lock).unwrap();
    out
}

#[tokio::test]
async fn a_channel_that_settles_before_the_lock_is_ours_is_left_alone() {
    // Review P1: the unlocked pre-check can catch an anchored append with its
    // dirty marker down. The writer finishes (clearing it) before repair gets
    // the lock, so repair must re-check there and move nothing.
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let _router = crate::channel_writer::plant_writer_identity(&home);
    let ch = ChannelService::open(&home)
        .unwrap()
        .create_for_workflow("g")
        .unwrap()
        .id;
    let parked = ask(&home, &ch).await.hitl_id.expect("parked");
    let log = std::fs::read(log_path(&home, &ch)).unwrap();

    let (h, c) = (home.clone(), ch.clone());
    let (h2, c2) = (home.clone(), ch.clone());
    let repaired = hold_lock_while(&home, &ch, move || {
        mur_channel::anchor::mark_dirty(&h, &c).unwrap();
        let worker = std::thread::spawn(move || repair(&h, &c, "tester").unwrap());
        // Let repair pass its unlocked check and block on the lock...
        std::thread::sleep(std::time::Duration::from_millis(300));
        // ...then the "writer" finishes its append.
        mur_channel::anchor::clear_dirty(&h2, &c2).unwrap();
        worker
    })
    .join()
    .unwrap();

    assert!(repaired.is_none(), "healthy channel was quarantined");
    assert_eq!(std::fs::read(log_path(&home, &ch)).unwrap(), log);
    assert!(!home.join("channels").join(&ch).join("quarantine").exists());
    assert!(!mur_channel::repair_marker::is_pending(&home, &ch));
    // The parked request survived.
    answer(&home, &ch, &parked, true);
}

#[tokio::test]
async fn a_repair_interrupted_after_the_move_reads_damaged_and_resumes() {
    // Review P1: crash after quarantine, before the repair record. Without
    // the marker this manifest-only channel reads intact and a rerun skips.
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let ch = damaged_channel(home).await;
    let store = ChannelStore::new(home);
    let marker = RepairMarker {
        label: "t-crash".into(),
        by: "tester".into(),
        before: serde_json::json!({"old_log_lines": 3}),
    };
    store
        .quarantine_log_if(&ch, &marker.label, || {
            repair_marker::write(home, &ch, &marker)?;
            Ok(Some(()))
        })
        .unwrap()
        .unwrap();
    // "Crash" here: log and head are gone, nothing written yet.

    let mid = channel_integrity::check(home, &ch).unwrap();
    assert!(!mid.is_intact(), "{}", mid.render());
    assert!(mid.repair_pending);
    // The gate trusts nothing here: it never allows, and whatever it parks
    // on this half-built log is quarantined again when the repair resumes.
    let gated = ask(home, &ch).await;
    assert!(!gated.allow, "gate must fail closed: {gated:?}");
    assert!(gated.reason.contains("interrupted"), "{gated:?}");

    let done = repair(home, &ch, "someone-else")
        .unwrap()
        .expect("resumed, not skipped");
    assert!(done.quarantine.ends_with("t-crash"), "same quarantine");
    assert!(done.after.is_intact(), "{}", done.after.render());
    assert!(!repair_marker::is_pending(home, &ch));
    let rec = &ChannelService::open(home)
        .unwrap()
        .load_events(&ch)
        .unwrap()[0];
    assert_eq!(
        rec.payload["repair"]["by"], "tester",
        "marker wins on resume"
    );

    let fresh = ask(home, &ch).await;
    assert!(fresh.deferred, "{fresh:?}");
    answer(home, &ch, fresh.hitl_id.as_deref().unwrap(), true);
    assert!(ask(home, &ch).await.allow);
}

#[tokio::test]
async fn a_missing_agent_key_is_refused_before_anything_moves() {
    // Review P1: an agent head we could not reseal would leave Gate B blocked.
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let ch = damaged_channel(home).await;
    let heads = home.join("agents/keyless").join(HITL_HEAD_DIR);
    std::fs::create_dir_all(&heads).unwrap();
    std::fs::write(heads.join(format!("{ch}.head")), b"stale").unwrap();
    let before = std::fs::read(log_path(home, &ch)).unwrap();

    let err = repair(home, &ch, "tester").unwrap_err();
    assert!(format!("{err:#}").contains("keyless"), "{err:#}");
    assert_eq!(std::fs::read(log_path(home, &ch)).unwrap(), before);
    assert!(heads.join(format!("{ch}.head")).exists());
    assert!(!repair_marker::is_pending(home, &ch));
    assert!(!home.join("channels").join(&ch).join("quarantine").exists());
}

#[tokio::test]
async fn a_repair_interrupted_before_a_reseal_resumes_and_reseals() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let ch = damaged_channel(home).await;
    let agent = crate::channel_writer::plant_identity_for(home, "helper");
    let heads = home.join("agents/helper").join(HITL_HEAD_DIR);
    std::fs::create_dir_all(&heads).unwrap();
    std::fs::write(heads.join(format!("{ch}.head")), b"stale").unwrap();
    // First run gets as far as moving the agent head, then "crashes".
    let marker = RepairMarker {
        label: "t-reseal".into(),
        by: "tester".into(),
        before: serde_json::Value::Null,
    };
    let (moved, ()) = ChannelStore::new(home)
        .quarantine_log_if(&ch, &marker.label, || {
            repair_marker::write(home, &ch, &marker)?;
            Ok(Some(()))
        })
        .unwrap()
        .unwrap();
    quarantine_agent_heads(home, &ch, &moved.dir).unwrap();
    assert!(!heads.join(format!("{ch}.head")).exists());

    let done = repair(home, &ch, "tester").unwrap().expect("resumed");
    let head = mur_channel::head_io::read_head(&heads, &ch)
        .unwrap()
        .unwrap();
    assert!(head.verify(&agent.verifying_key_bytes()));
    assert_eq!(
        head.lines,
        mur_channel::head::chain_log(&std::fs::read(log_path(home, &ch)).unwrap()).lines
    );
    assert!(done.after.is_intact(), "{}", done.after.render());
}

#[tokio::test]
async fn a_writer_racing_the_repair_lands_after_the_repair_record() {
    // Review P1: the lock must span move, first event and reseals. A writer
    // that slips in between would start the fresh chain itself (or have the
    // line it wrote quarantined), so the record would not open the chain.
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let ch = damaged_channel(&home).await;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (h, c, s) = (home.clone(), ch.clone(), stop.clone());
    let writer = std::thread::spawn(move || {
        let svc = ChannelService::open(&h).unwrap();
        let mut n = 0u32;
        while !s.load(std::sync::atomic::Ordering::Relaxed) {
            // Errors are fine while the channel is damaged; only what lands
            // on the fresh chain matters.
            let _ = crate::channel_writer::append_as_writer(
                &svc,
                &h,
                &c,
                ROUTER_AGENT,
                ChannelActor::System,
                EventKind::Note,
                serde_json::json!({ "text": format!("racer {n}") }),
                None,
            );
            n += 1;
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    let done = repair(&home, &ch, "tester").unwrap().expect("repaired");
    std::thread::sleep(std::time::Duration::from_millis(50));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();

    let events = ChannelService::open(&home)
        .unwrap()
        .load_events(&ch)
        .unwrap();
    assert_eq!(events[0].seq, done.repair_seq);
    assert_eq!(events[0].seq, 0, "repair record opens the fresh chain");
    assert!(events[0].payload.get("repair").is_some(), "{:?}", events[0]);
    let after = channel_integrity::check(&home, &ch).unwrap();
    assert!(after.is_intact(), "{}", after.render());
}
