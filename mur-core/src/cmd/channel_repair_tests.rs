//! `mur channel repair` against real fixtures: the router identity is planted,
//! events go through the production writer (so heads are anchored), and damage
//! is done to the files the way a bad disk or an attacker would.

use super::*;
use crate::cmd::channel_integrity::check;
use crate::hitl::gate::{ActionRequest, GatePolicy, gate};
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

#[test]
fn a_gate_b_decision_channel_is_refused() {
    let (tmp, ch) = setup();
    note(tmp.path(), &ch, 0);
    mur_channel::anchor::mark_dirty(tmp.path(), &ch).unwrap();
    let agent = tmp.path().join("agents").join("qa");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join(HITL_CHANNEL_FILE), &ch).unwrap();
    let err = repair(tmp.path(), &ch, Confirm::Yes).unwrap_err();
    assert!(format!("{err:#}").contains("qa"), "{err:#}");
    assert!(mur_channel::anchor::is_dirty(tmp.path(), &ch), "untouched");
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
