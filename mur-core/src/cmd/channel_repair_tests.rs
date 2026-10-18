//! `mur channel repair` against real fixtures: the router identity is planted,
//! events go through the production writer (so heads are anchored), and damage
//! is done to the files the way a bad disk or an attacker would.

use super::*;
use crate::channel_writer::append_as_writer;
use crate::cmd::channel_integrity::check;
use crate::hitl::gate::{ActionRequest, GatePolicy, gate};
use mur_common::channel::EventKind;
use mur_common::hitl::{RiskTier, Unanswered};
use tempfile::TempDir;

fn setup() -> (TempDir, String) {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("repair")
        .unwrap();
    (tmp, ch.id)
}

fn note(home: &Path, ch: &str, i: usize) {
    let svc = ChannelService::open(home).unwrap();
    append_as_writer(
        &svc,
        home,
        ch,
        ROUTER_AGENT,
        ChannelActor::System,
        EventKind::Note,
        serde_json::json!({ "text": format!("event {i}") }),
        None,
    )
    .unwrap();
}

fn log_path(home: &Path, ch: &str) -> PathBuf {
    mur_channel::ChannelStore::new(home)
        .events_path(ch)
        .unwrap()
}

/// Cut the last line: the log is now truncated below its signed head.
fn truncate_last_line(home: &Path, ch: &str) {
    let path = log_path(home, ch);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines.pop();
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(&path, out).unwrap();
}

fn ask_with(answer: &str) -> (std::io::Cursor<Vec<u8>>, bool) {
    (std::io::Cursor::new(answer.as_bytes().to_vec()), true)
}

#[test]
fn an_intact_channel_is_a_no_op() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    let before = std::fs::read(log_path(tmp.path(), &ch)).unwrap();
    let out = repair(tmp.path(), &ch, Confirm::Yes).unwrap();
    assert!(matches!(out, Outcome::Intact), "{out:?}");
    assert_eq!(std::fs::read(log_path(tmp.path(), &ch)).unwrap(), before);
    assert_eq!(run(tmp.path(), &ch, true).unwrap(), 0);
}

#[test]
fn a_damaged_channel_is_quarantined_and_verifies_intact_after() {
    let (tmp, ch) = setup();
    for i in 0..3 {
        note(tmp.path(), &ch, i);
    }
    truncate_last_line(tmp.path(), &ch);
    let damaged = std::fs::read(log_path(tmp.path(), &ch)).unwrap();
    assert!(!check(tmp.path(), &ch).unwrap().is_intact());

    let Outcome::Repaired { quarantined, seq } = repair(tmp.path(), &ch, Confirm::Yes).unwrap()
    else {
        panic!("expected a repair");
    };
    let quarantined = quarantined.expect("the damaged log was moved aside");
    assert_eq!(
        std::fs::read(&quarantined).unwrap(),
        damaged,
        "preserved byte for byte"
    );
    assert_eq!(seq, 0, "the repair event opens the fresh chain");

    let report = check(tmp.path(), &ch).unwrap();
    assert!(report.is_intact(), "{}", report.render());
    assert_eq!(
        report.anchored_lines,
        Some(1),
        "the fresh chain is anchored"
    );

    // The repair event names the quarantined file and the old head.
    let evs = ChannelService::open(tmp.path())
        .unwrap()
        .load_events(&ch)
        .unwrap();
    assert_eq!(evs.len(), 1);
    let p = &evs[0].payload;
    assert_eq!(p["type"], REPAIR_EVENT_TYPE);
    assert_eq!(
        p["quarantined_log"].as_str().unwrap(),
        quarantined.file_name().unwrap().to_string_lossy()
    );
    assert_eq!(p["old_head"]["lines"], 3);
    assert!(p["reason"].as_str().unwrap().contains("Truncated"));
}

#[test]
fn a_dirty_marker_alone_is_repaired() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    mur_channel::anchor::mark_dirty(tmp.path(), &ch).unwrap();
    assert!(matches!(
        repair(tmp.path(), &ch, Confirm::Yes).unwrap(),
        Outcome::Repaired { .. }
    ));
    assert!(!mur_channel::anchor::is_dirty(tmp.path(), &ch));
    assert!(check(tmp.path(), &ch).unwrap().is_intact());
}

#[test]
fn an_unreadable_head_is_repaired() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    let head =
        mur_channel::head_io::head_path(&mur_channel::anchor::heads_dir(tmp.path()), &ch).unwrap();
    std::fs::write(head, b"not json").unwrap();
    assert!(matches!(
        repair(tmp.path(), &ch, Confirm::Yes).unwrap(),
        Outcome::Repaired { .. }
    ));
    assert!(check(tmp.path(), &ch).unwrap().is_intact());
}

#[test]
fn non_interactive_without_yes_is_refused_and_touches_nothing() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    note(tmp.path(), &ch, 1);
    truncate_last_line(tmp.path(), &ch);
    let before = std::fs::read(log_path(tmp.path(), &ch)).unwrap();
    let mut input = std::io::Cursor::new(b"y\n".to_vec());
    let err = repair(
        tmp.path(),
        &ch,
        Confirm::Ask {
            input: &mut input,
            interactive: false,
        },
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("--yes"), "{err:#}");
    assert_eq!(std::fs::read(log_path(tmp.path(), &ch)).unwrap(), before);
}

#[test]
fn declining_the_prompt_touches_nothing() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    note(tmp.path(), &ch, 1);
    truncate_last_line(tmp.path(), &ch);
    let before = std::fs::read(log_path(tmp.path(), &ch)).unwrap();
    let (mut input, interactive) = ask_with("n\n");
    let out = repair(
        tmp.path(),
        &ch,
        Confirm::Ask {
            input: &mut input,
            interactive,
        },
    )
    .unwrap();
    assert!(matches!(out, Outcome::Declined));
    assert_eq!(std::fs::read(log_path(tmp.path(), &ch)).unwrap(), before);
}

/// Every file named `name` under `dir` (the private key lives in a
/// platform-specific subdirectory, so it is found rather than guessed).
fn find_files(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            find_files(&p, name, out);
        } else if p.file_name().is_some_and(|n| n == name) {
            out.push(p);
        }
    }
}

/// Review P2 on #1973: with the router's PRIVATE key gone but its public key
/// still there, the damage check runs, so the old code quarantined the log,
/// dropped the head, and then fell back to an UNSIGNED repair record —
/// reporting success over a chain nothing can verify. It must refuse first.
#[test]
fn without_the_router_private_key_it_refuses_and_touches_nothing() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    note(tmp.path(), &ch, 1);
    truncate_last_line(tmp.path(), &ch);
    let mut keys = Vec::new();
    find_files(tmp.path(), "identity.key", &mut keys);
    assert!(!keys.is_empty(), "the planted router key must be on disk");
    for k in &keys {
        std::fs::remove_file(k).unwrap();
    }
    let heads = mur_channel::anchor::heads_dir(tmp.path());
    let head_before = mur_channel::head_io::read_head(&heads, &ch).unwrap();
    assert!(head_before.is_some());
    let log_before = std::fs::read(log_path(tmp.path(), &ch)).unwrap();

    let err = repair(tmp.path(), &ch, Confirm::Yes).unwrap_err();

    assert!(format!("{err:#}").contains("signing key"), "{err:#}");
    assert_eq!(
        std::fs::read(log_path(tmp.path(), &ch)).unwrap(),
        log_before
    );
    assert_eq!(
        mur_channel::head_io::read_head(&heads, &ch).unwrap(),
        head_before,
        "the old head must not be dropped"
    );
    let dir = log_path(tmp.path(), &ch).parent().unwrap().to_path_buf();
    let quarantined = std::fs::read_dir(&dir).unwrap().flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .starts_with("events.quarantined-")
    });
    assert!(!quarantined, "nothing may be moved aside");
}

/// The repaired chain is signed and anchored: seq 0 carries a signature and
/// the router head exists, so `verify` reports intact.
#[test]
fn the_repair_record_is_signed_and_anchored() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    note(tmp.path(), &ch, 1);
    truncate_last_line(tmp.path(), &ch);
    repair(tmp.path(), &ch, Confirm::Yes).unwrap();
    let text = std::fs::read_to_string(log_path(tmp.path(), &ch)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let ev: mur_common::channel::ChannelEvent = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(ev.seq, 0);
    assert!(ev.sig.is_some(), "the repair record must be signed");
    let heads = mur_channel::anchor::heads_dir(tmp.path());
    assert!(
        mur_channel::head_io::read_head(&heads, &ch)
            .unwrap()
            .is_some()
    );
    assert!(check(tmp.path(), &ch).unwrap().is_intact());
}

// --- Gate B: the agent's own decision channel ---------------------------

mod gate_b {
    use super::*;
    use mur_agent_runtime::hitl::store::{ChannelDecisionStore, DecisionStore, Settled};
    use mur_common::hitl::HitlResponse;
    use mur_common::identity::AgentIdentity;
    use std::sync::Arc;

    const AGENT: &str = "qa";

    /// A home whose agent key is on disk, as `mur agent create` leaves it.
    fn agent_store(home: &Path) -> ChannelDecisionStore {
        let dir = home.join("agents").join(AGENT);
        std::fs::create_dir_all(&dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&dir).unwrap();
        ChannelDecisionStore::new(home.to_path_buf(), AGENT.into(), Arc::new(id), 0)
    }

    fn channel(home: &Path) -> String {
        std::fs::read_to_string(home.join("agents").join(AGENT).join(HITL_CHANNEL_FILE))
            .unwrap()
            .trim()
            .to_string()
    }

    fn resp(hash: &str, id: &str) -> HitlResponse {
        HitlResponse {
            hitl_id: id.into(),
            action_hash: hash.into(),
            allow: true,
            reason: String::new(),
            surface: "hub".into(),
            issued_at: Some(chrono::Utc::now()),
            label: None,
        }
    }

    #[tokio::test]
    async fn a_damaged_decision_channel_is_resealed_by_the_agent_key() {
        let tmp = TempDir::new().unwrap();
        let s = agent_store(tmp.path());
        s.record(resp("h0", "a")).await;
        s.record(resp("h1", "b")).await;
        let ch = channel(tmp.path());
        // A deleted record: Gate B now refuses every remembered approval.
        truncate_last_line(tmp.path(), &ch);
        assert_eq!(s.lookup("h0").await, None, "damaged log is not trusted");

        let Outcome::Repaired { quarantined, seq } = repair(tmp.path(), &ch, Confirm::Yes).unwrap()
        else {
            panic!("expected a repair");
        };
        assert_eq!(seq, 0);
        assert!(quarantined.is_some_and(|p| p.exists()));

        // The fresh chain carries the agent seal, so Gate B trusts it again —
        // and nothing from before the repair came back.
        assert_eq!(s.lookup("h0").await, None, "no decision carried over");
        s.record(resp("h2", "c")).await;
        assert_eq!(s.lookup("h2").await, Some(Settled::Allow));
        assert_eq!(channel(tmp.path()), ch, "the same channel, not a rollover");

        // A second repair finds nothing to do.
        assert!(matches!(
            repair(tmp.path(), &ch, Confirm::Yes).unwrap(),
            Outcome::Intact
        ));
    }

    #[tokio::test]
    async fn an_intact_decision_channel_is_a_no_op() {
        let tmp = TempDir::new().unwrap();
        let s = agent_store(tmp.path());
        s.record(resp("h0", "a")).await;
        let ch = channel(tmp.path());
        let before = std::fs::read(log_path(tmp.path(), &ch)).unwrap();
        assert!(matches!(
            repair(tmp.path(), &ch, Confirm::Yes).unwrap(),
            Outcome::Intact
        ));
        assert_eq!(std::fs::read(log_path(tmp.path(), &ch)).unwrap(), before);
    }

    /// Gate B too: a decision channel whose repair died part-way is refused
    /// by the runtime's own check, and a rerun reseals it.
    #[tokio::test]
    async fn an_interrupted_decision_channel_repair_is_refused_then_resumed() {
        let tmp = TempDir::new().unwrap();
        let s = agent_store(tmp.path());
        s.record(resp("h0", "a")).await;
        let ch = channel(tmp.path());
        mur_channel::repair_marker::mark(tmp.path(), &ch, "t0").unwrap();
        let log = log_path(tmp.path(), &ch);
        std::fs::rename(&log, log.with_file_name("events.quarantine-crash.jsonl")).unwrap();
        mur_channel::head_io::remove_head(
            &mur_agent_runtime::hitl::integrity::heads_dir(tmp.path(), AGENT),
            &ch,
        )
        .unwrap();
        let key = AgentIdentity::load(&tmp.path().join("agents").join(AGENT))
            .unwrap()
            .verifying_key_bytes();
        assert!(
            mur_agent_runtime::hitl::integrity::verify(tmp.path(), AGENT, &ch, &key).is_err(),
            "an empty, headless log under a pending repair must not pass Gate B"
        );

        assert!(matches!(
            repair(tmp.path(), &ch, Confirm::Yes).unwrap(),
            Outcome::Repaired { seq: 0, .. }
        ));
        assert!(!mur_channel::repair_marker::is_pending(tmp.path(), &ch));
        s.record(resp("h1", "b")).await;
        assert_eq!(s.lookup("h1").await, Some(Settled::Allow));
    }

    #[test]
    fn without_the_agent_key_it_refuses_and_touches_nothing() {
        let (tmp, ch) = setup();
        note(tmp.path(), &ch, 0);
        mur_channel::anchor::mark_dirty(tmp.path(), &ch).unwrap();
        let agent = tmp.path().join("agents").join(AGENT);
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join(HITL_CHANNEL_FILE), &ch).unwrap();
        let before = std::fs::read(log_path(tmp.path(), &ch)).unwrap();
        let err = repair(tmp.path(), &ch, Confirm::Yes).unwrap_err();
        assert!(format!("{err:#}").contains("signing key"), "{err:#}");
        assert!(mur_channel::anchor::is_dirty(tmp.path(), &ch), "untouched");
        assert_eq!(std::fs::read(log_path(tmp.path(), &ch)).unwrap(), before);
    }
}

/// Leave `ch` the way a repair killed part-way leaves it: marker down, log
/// moved aside, old head dropped, and no fresh record yet.
fn crash_mid_repair(home: &Path, ch: &str) {
    mur_channel::repair_marker::mark(home, ch, "t0").unwrap();
    let log = log_path(home, ch);
    std::fs::rename(&log, log.with_file_name("events.quarantine-crash.jsonl")).unwrap();
    mur_channel::head_io::remove_head(&mur_channel::anchor::heads_dir(home), ch).unwrap();
}

/// Review P1 on #1968: without a marker, a repair that died after the move
/// left an empty, headless log that verified as a fresh intact channel, and
/// a rerun took the intact shortcut. Now it reads as damaged, and a rerun
/// finishes the repair.
#[test]
fn an_interrupted_repair_reads_damaged_and_a_rerun_finishes_it() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    note(tmp.path(), &ch, 1);
    crash_mid_repair(tmp.path(), &ch);

    let r = check(tmp.path(), &ch).unwrap();
    assert!(r.repair_pending && !r.is_intact(), "{r:?}");
    assert!(r.render().contains("did not finish"));

    let Outcome::Repaired { seq, .. } = repair(tmp.path(), &ch, Confirm::Yes).unwrap() else {
        panic!("a rerun must resume, not take the intact shortcut");
    };
    assert_eq!(seq, 0);
    assert!(!mur_channel::repair_marker::is_pending(tmp.path(), &ch));
    let after = check(tmp.path(), &ch).unwrap();
    assert!(after.is_intact(), "{after:?}");
    let events = mur_channel::ChannelStore::new(tmp.path())
        .load_events(&ch)
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["type"], REPAIR_EVENT_TYPE);
}

/// A finished repair leaves no marker behind.
#[test]
fn a_completed_repair_clears_its_marker() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    note(tmp.path(), &ch, 1);
    truncate_last_line(tmp.path(), &ch);
    repair(tmp.path(), &ch, Confirm::Yes).unwrap();
    assert!(!mur_channel::repair_marker::is_pending(tmp.path(), &ch));
    assert!(check(tmp.path(), &ch).unwrap().is_intact());
}

#[test]
fn an_unknown_channel_is_an_error() {
    let tmp = TempDir::new().unwrap();
    assert!(repair(tmp.path(), "nope", Confirm::Yes).is_err());
}

fn action() -> ActionRequest {
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

/// The issue's acceptance: damaged → repair → the next gated action asks
/// once, and that answer is reused afterwards. A decision given on the
/// damaged log before the repair is not carried over.
#[tokio::test]
async fn after_repair_the_gate_asks_once_and_reuses_the_answer() {
    let (tmp, ch) = setup();
    let home = tmp.path();
    let a = action();

    // Ask, approve, and confirm the answer is reused.
    let d = gate(home, &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(d.deferred, "{d:?}");
    crate::cmd::channel::approve_in(home, &ch, d.hitl_id.as_deref().unwrap(), false, None).unwrap();
    assert!(
        gate(home, &ch, &a, &unattended(), None, None)
            .await
            .unwrap()
            .allow
    );

    // Damage the log; now nothing is reused, even a fresh approval.
    note(home, &ch, 0);
    truncate_last_line(home, &ch);
    let d = gate(home, &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "{d:?}");
    if let Some(id) = d.hitl_id.as_deref() {
        crate::cmd::channel::approve_in(home, &ch, id, false, None).unwrap();
    }
    assert!(
        !gate(home, &ch, &a, &unattended(), None, None)
            .await
            .unwrap()
            .allow
    );

    // Repair: the gate asks once more (nothing carried over) …
    assert!(matches!(
        repair(home, &ch, Confirm::Yes).unwrap(),
        Outcome::Repaired { .. }
    ));
    let d = gate(home, &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow && d.deferred, "asks again after repair: {d:?}");
    assert!(
        !d.reason.contains("integrity"),
        "the fresh chain is clean: {d:?}"
    );
    crate::cmd::channel::approve_in(home, &ch, d.hitl_id.as_deref().unwrap(), false, None).unwrap();

    // … and that answer is reused.
    let d = gate(home, &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(d.allow, "the post-repair approval is reused: {d:?}");
    assert!(check(home, &ch).unwrap().is_intact());
}
