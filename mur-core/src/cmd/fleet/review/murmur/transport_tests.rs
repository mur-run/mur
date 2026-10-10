//! Tests for `MurmurTransport` (P3b-§4.1, §6.3; AC-P3b-8…12, 15, 17–21, 25, 31).
//! The UI side is a scripted receiver; the network edges are injected.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::super::driver::{ReviewTransport, SendAnswer};
use super::super::ledger::{EscalationRecord, Ledger};
use super::super::schema::PauseKind;
use super::super::turn_cell::TurnState;
use super::bridge::{DriverReq, ReviewFlags};
use super::transport::{DialFn, MurmurTransport, RespondFn};

const MEMBERS: [&str; 2] = ["main-x", "rev-x"];

fn members() -> [String; 2] {
    MEMBERS.map(String::from)
}

fn task(reply: &str) -> serde_json::Value {
    json!({"state": "completed", "messages": [
        {"role": "agent", "parts": [{"text": reply}]}]})
}

fn dial_ok(reply: &'static str) -> DialFn {
    Arc::new(move |_, _, _, on_delta, _| {
        on_delta("partial ", false, "TASK-7");
        Ok(task(reply))
    })
}

fn no_respond() -> RespondFn {
    Arc::new(|_, _, _| {})
}

fn transport(dial: DialFn, respond: RespondFn) -> (MurmurTransport, Receiver<DriverReq>) {
    let (tx, rx) = channel();
    let t = MurmurTransport::with_io(
        std::env::temp_dir(),
        tx,
        ReviewFlags::default(),
        members(),
        dial,
        respond,
    );
    (t, rx)
}

fn tag(r: &DriverReq) -> &'static str {
    match r {
        DriverReq::Confirm { .. } => "Confirm",
        DriverReq::Ruling { .. } => "Ruling",
        DriverReq::Show(_) => "Show",
        DriverReq::TurnStarted { .. } => "TurnStarted",
        DriverReq::TurnEnded { .. } => "TurnEnded",
        DriverReq::Hitl { .. } => "Hitl",
    }
}

#[test]
fn confirm_forwards_the_answer_and_marks_turn_boundaries() {
    let (t, rx) = transport(dial_ok("REPLY-A"), no_respond());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let ui = std::thread::spawn(move || {
        while let Ok(r) = rx.recv() {
            log.lock().unwrap().push(tag(&r));
            if let DriverReq::Confirm { reply, .. } = r {
                reply.send(SendAnswer::Send).unwrap();
            }
        }
    });
    let open = BTreeSet::new();
    let params = json!({});
    let answer = t.confirm_send("main-x", &params, &open).unwrap();
    assert_eq!(answer, SendAnswer::Send);
    assert_eq!(t.send("main-x", &params).unwrap(), "REPLY-A");
    drop(t);
    ui.join().unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        ["Confirm", "TurnStarted", "TurnEnded"]
    );
}

#[test]
fn confirm_pause_requested_when_receiver_dropped() {
    let (t, rx) = transport(dial_ok("x"), no_respond());
    drop(rx);
    let answer = t
        .confirm_send("main-x", &json!({}), &BTreeSet::new())
        .unwrap();
    assert_eq!(answer, SendAnswer::Stop);
    assert!(t.detached());
    assert_eq!(t.pause_kind(), Some(PauseKind::Detached));
}

#[test]
fn confirm_detached_when_the_reply_is_never_sent() {
    let (t, rx) = transport(dial_ok("x"), no_respond());
    let ui = std::thread::spawn(move || drop(rx.recv().unwrap()));
    let answer = t
        .confirm_send("main-x", &json!({}), &BTreeSet::new())
        .unwrap();
    ui.join().unwrap();
    assert_eq!(answer, SendAnswer::Stop);
    assert!(t.detached());
}

#[test]
fn confirm_pause_requested_flag_yields_pause() {
    let (t, rx) = transport(dial_ok("x"), no_respond());
    t.flags.pause_requested.store(true, Ordering::Release);
    let answer = t
        .confirm_send("main-x", &json!({}), &BTreeSet::new())
        .unwrap();
    assert_eq!(answer, SendAnswer::Stop);
    assert_eq!(t.pause_kind(), Some(PauseKind::User));
    assert!(!t.detached());
    assert!(rx.try_recv().is_err(), "no Confirm may be sent");
}

#[test]
fn send_stores_task_id_in_turn_started_slot() {
    let (t, rx) = transport(dial_ok("R"), no_respond());
    t.send("main-x", &json!({})).unwrap();
    let DriverReq::TurnStarted { task_id, .. } = rx.recv().unwrap() else {
        panic!("first request must be TurnStarted");
    };
    assert_eq!(task_id.get().map(String::as_str), Some("TASK-7"));
}

#[test]
fn send_commits_cell_before_turn_ended() {
    let (t, rx) = transport(dial_ok("R"), no_respond());
    let ui = std::thread::spawn(move || {
        let DriverReq::TurnStarted { turn, .. } = rx.recv().unwrap() else {
            panic!("expected TurnStarted");
        };
        assert!(matches!(rx.recv().unwrap(), DriverReq::TurnEnded { .. }));
        turn.state()
    });
    t.send("main-x", &json!({})).unwrap();
    assert!(t.turn_committed());
    assert_eq!(ui.join().unwrap(), TurnState::Committed);
}

#[test]
fn send_aborted_cell_yields_turn_committed_false() {
    // The dial holds the reply until the UI has aborted the turn, so the
    // abort deterministically wins the CAS (AC-P3b-20).
    let (go_tx, go_rx) = channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let dial: DialFn = Arc::new(move |_, _, _, _, _| {
        go_rx.lock().unwrap().recv().unwrap();
        Ok(task("REPLY-X"))
    });
    let (t, rx) = transport(dial, no_respond());
    let ui = std::thread::spawn(move || {
        let DriverReq::TurnStarted { turn, .. } = rx.recv().unwrap() else {
            panic!("expected TurnStarted");
        };
        assert!(turn.abort(), "abort must win while the turn is in flight");
        go_tx.send(()).unwrap();
        turn
    });
    t.send("main-x", &json!({})).unwrap();
    assert!(!t.turn_committed());
    assert_eq!(ui.join().unwrap().state(), TurnState::Aborted);
}

#[test]
fn ruling_returns_line_verbatim_and_empty_on_abort() {
    let ledger = Ledger::default();
    let pending = EscalationRecord {
        finding_id: "F1".into(),
        reason: "stuck".into(),
        handled: false,
    };
    for line in ["/rule drop F1 RULING-TXT", ""] {
        let (t, rx) = transport(dial_ok("x"), no_respond());
        let ui = std::thread::spawn(move || {
            let DriverReq::Ruling { reply, .. } = rx.recv().unwrap() else {
                panic!("expected Ruling");
            };
            reply.send(line.to_string()).unwrap();
        });
        assert_eq!(t.ask_ruling(&pending, &ledger).unwrap(), line);
        ui.join().unwrap();
    }
}

#[test]
fn hitl_reply_bool_then_worker_responds() {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let got = answers.clone();
    let respond: RespondFn = Arc::new(move |m, id, allow| {
        got.lock()
            .unwrap()
            .push((m.to_string(), id.to_string(), allow));
    });
    let dial: DialFn = Arc::new(|_, _, _, _, on_hitl| {
        on_hitl(json!({"calls": [
            {"hitl_id": "H-1", "tool_name": "bash", "tool_input": {}},
            {"hitl_id": "H-2", "tool_name": "bash", "tool_input": {}}]}));
        Ok(task("R"))
    });
    let (t, rx) = transport(dial, respond);
    let ui = std::thread::spawn(move || {
        let mut hitls = 0;
        while let Ok(r) = rx.recv() {
            if let DriverReq::Hitl { req, reply, .. } = r {
                hitls += 1;
                reply.send(req.hitl_id == "H-2").unwrap();
            }
        }
        hitls
    });
    t.send("main-x", &json!({})).unwrap();
    drop(t);
    assert_eq!(ui.join().unwrap(), 2, "one Hitl request per call");
    assert_eq!(
        *answers.lock().unwrap(),
        [
            ("main-x".to_string(), "H-1".to_string(), false),
            ("main-x".to_string(), "H-2".to_string(), true)
        ]
    );
}

#[test]
fn hitl_denies_when_the_ui_is_gone() {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let got = answers.clone();
    let respond: RespondFn = Arc::new(move |_, id, allow| {
        got.lock().unwrap().push((id.to_string(), allow));
    });
    let dial: DialFn = Arc::new(|_, _, _, _, on_hitl| {
        on_hitl(json!({"hitl_id": "H-9", "tool_name": "bash", "tool_input": {}}));
        Ok(task("R"))
    });
    let (t, rx) = transport(dial, respond);
    drop(rx);
    t.send("main-x", &json!({})).unwrap();
    assert_eq!(*answers.lock().unwrap(), [("H-9".to_string(), false)]);
}

#[test]
fn gate_wait_is_recorded_then_reset() {
    let (t, rx) = transport(dial_ok("x"), no_respond());
    let ui = std::thread::spawn(move || {
        let DriverReq::Confirm { reply, .. } = rx.recv().unwrap() else {
            panic!("expected Confirm");
        };
        std::thread::sleep(std::time::Duration::from_millis(30));
        reply.send(SendAnswer::Send).unwrap();
    });
    t.confirm_send("main-x", &json!({}), &BTreeSet::new())
        .unwrap();
    ui.join().unwrap();
    assert!(t.take_human_wait() >= std::time::Duration::from_millis(30));
    assert_eq!(t.take_human_wait(), std::time::Duration::ZERO);
}
