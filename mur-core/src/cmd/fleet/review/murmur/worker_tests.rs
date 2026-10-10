//! Tests for the review worker (P3b-§4.3, §4.4, §8; AC-P3b-4a, 16, 20b, 23a,
//! 26, 29). The UI side is the test thread or a scripted thread; the network
//! edges are injected, so nothing here touches a real agent.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use mur_channel::ChannelService;
use mur_common::channel::EventKind;
use mur_common::fleet::Fleet;
use mur_common::limits::Stuck;
use serde_json::json;

use super::super::constants::{
    REVIEW_AUTO_DEGRADED_NOTICE, REVIEW_PAUSE_REASON_DETACHED, REVIEW_PAUSE_REASON_USER,
};
use super::super::driver::SendAnswer;
use super::super::loop_driver::{LoopDriverStop, run_review_loop};
use super::super::resume::prepare_resume;
use super::super::run_lock::{self, LockDenied};
use super::super::schema::{
    Mode, NoteClassification, PauseKind, ReviewPayload, SessionLimits, classify_note_payload,
};
use super::super::session::create_session_fleet;
use super::super::wire::message_text;
use super::bridge::{DriverEvent, DriverReq, Outcome};
use super::transport::{DialFn, RespondFn};
use super::worker::{StartKind, WorkerHandle, spawn_with_io};

const APPROVE: &str = r#"{"verdict":"approve"}"#;
const ISSUE_F1: &str =
    r#"{"verdict":"revise","findings":[{"severity":"high","issue":"unchecked unwrap"}]}"#;
const DISPUTE_F1: &str =
    r#"{"verdict":"revise","prior":[{"id":"F1","status":"disputed","reason":"still panics"}]}"#;

fn limits() -> SessionLimits {
    SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None)
}

fn task(reply: &str) -> serde_json::Value {
    json!({"state": "completed", "messages": [
        {"role": "agent", "parts": [{"text": reply}]}]})
}

/// Scripted members: `main` answers every listed finding with `answer`; the
/// reviewer pops `reviewer` (then approves). `hold_main` blocks main's turn
/// until the UI side lets it go.
struct Script {
    answer: &'static str,
    reviewer: Mutex<Vec<&'static str>>,
    main_calls: AtomicUsize,
    hold_main: Mutex<Option<Receiver<()>>>,
}

impl Script {
    fn new(answer: &'static str, reviewer: &[&'static str]) -> Arc<Self> {
        Arc::new(Self {
            answer,
            reviewer: Mutex::new(reviewer.iter().rev().copied().collect()),
            main_calls: AtomicUsize::new(0),
            hold_main: Mutex::new(None),
        })
    }

    fn reply(&self, member: &str, params: &serde_json::Value) -> String {
        if member == "reviewer" {
            return self
                .reviewer
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(APPROVE)
                .into();
        }
        self.main_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(rx) = self.hold_main.lock().unwrap().take() {
            let _ = rx.recv();
        }
        let responses: Vec<serde_json::Value> = message_text(params)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("- "))
            .filter_map(|l| l.split_once(" ["))
            .map(|(id, _)| id)
            .filter(|id| id.starts_with('F'))
            .map(|id| json!({"id": id, "answer": self.answer, "reason": "r"}))
            .collect();
        format!("done\n```json\n{}\n```", json!({ "responses": responses }))
    }

    fn dial(self: &Arc<Self>) -> DialFn {
        let me = self.clone();
        Arc::new(move |_, member, params, on_delta, _| {
            on_delta("x", false, "T-1");
            Ok(task(&me.reply(member, &params)))
        })
    }
}

fn no_respond() -> RespondFn {
    Arc::new(|_, _, _| {})
}

fn home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    tmp
}

fn payloads(home: &Path, channel_id: &str) -> Vec<ReviewPayload> {
    ChannelService::open(home)
        .unwrap()
        .load_events(channel_id)
        .unwrap()
        .into_iter()
        .filter(|ev| ev.kind == EventKind::Note)
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some(env.payload),
            _ => None,
        })
        .collect()
}

fn fresh(
    home: &Path,
    name: &str,
    script: &Arc<Script>,
) -> (
    WorkerHandle,
    Receiver<DriverReq>,
    Receiver<DriverEvent>,
    Fleet,
) {
    let fleet = create_session_fleet(home, name, "main", "reviewer", "the task").unwrap();
    let svc = ChannelService::open(home).unwrap();
    let lock = run_lock::try_acquire(&svc, &fleet.channel_id).unwrap();
    let (req, req_rx) = channel();
    let (done, done_rx) = channel();
    let kind = StartKind::Fresh {
        fleet: Box::new(fleet.clone()),
        task: "the task".into(),
        limits: limits(),
        lock,
    };
    let h = spawn_with_io(
        home.to_path_buf(),
        kind,
        req,
        done,
        script.dial(),
        no_respond(),
    )
    .unwrap();
    (h, req_rx, done_rx, fleet)
}

fn resumed(
    home: &Path,
    name: &str,
    script: &Arc<Script>,
) -> (WorkerHandle, Receiver<DriverReq>, Receiver<DriverEvent>) {
    let r = prepare_resume(home, name).unwrap();
    let (req, req_rx) = channel();
    let (done, done_rx) = channel();
    let h = spawn_with_io(
        home.to_path_buf(),
        StartKind::Resume(Box::new(r)),
        req,
        done,
        script.dial(),
        no_respond(),
    )
    .unwrap();
    (h, req_rx, done_rx)
}

fn finish(h: WorkerHandle, done_rx: &Receiver<DriverEvent>) -> Outcome {
    let DriverEvent::Finished(out) = done_rx.recv().expect("the worker reports once");
    h.join.join().unwrap();
    out
}

/// A UI that sends every turn and rules nothing; returns what it saw.
fn ui_send_all(rx: Receiver<DriverReq>) -> JoinHandle<Vec<&'static str>> {
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        while let Ok(r) = rx.recv() {
            seen.push(match r {
                DriverReq::Confirm { reply, .. } => {
                    let _ = reply.send(SendAnswer::Send);
                    "Confirm"
                }
                DriverReq::Ruling { reply, .. } => {
                    let _ = reply.send(String::new());
                    "Ruling"
                }
                DriverReq::Hitl { reply, .. } => {
                    let _ = reply.send(false);
                    "Hitl"
                }
                DriverReq::Show(_) => "Show",
                DriverReq::TurnStarted { .. } => "TurnStarted",
                DriverReq::TurnEnded { .. } => "TurnEnded",
            });
        }
        seen
    })
}

fn turn_sents(all: &[ReviewPayload]) -> usize {
    all.iter()
        .filter(|p| matches!(p, ReviewPayload::TurnSent { .. }))
        .count()
}

fn paused(all: &[ReviewPayload]) -> Vec<(PauseKind, String, u64)> {
    all.iter()
        .filter_map(|p| match p {
            ReviewPayload::Paused {
                kind,
                reason,
                human_wait_ms,
                ..
            } => Some((*kind, reason.clone(), *human_wait_ms)),
            _ => None,
        })
        .collect()
}

#[test]
fn lock_is_free_only_after_join() {
    let tmp = home();
    let script = Script::new("accept", &[APPROVE]);
    let (h, req_rx, done_rx, fleet) = fresh(tmp.path(), "review-wrk00001", &script);
    let svc = ChannelService::open(tmp.path()).unwrap();

    // The worker is parked at its first gate: it owns the lock.
    let DriverReq::Confirm { reply, .. } = req_rx.recv().unwrap() else {
        panic!("the first request is the send gate");
    };
    assert!(matches!(
        run_lock::try_acquire(&svc, &fleet.channel_id),
        Err(LockDenied::Running(_))
    ));
    reply.send(SendAnswer::Stop).unwrap();
    while req_rx.recv().is_ok() {}

    let out = finish(h, &done_rx);
    assert!(
        matches!(out, Outcome::Ran(LoopDriverStop::Stopped, ..)),
        "{out:?}"
    );
    assert!(run_lock::try_acquire(&svc, &fleet.channel_id).is_ok());
}

#[test]
fn finished_event_carries_ran_outcome() {
    let tmp = home();
    let script = Script::new("accept", &[APPROVE]);
    let (h, req_rx, done_rx, fleet) = fresh(tmp.path(), "review-wrk00002", &script);
    let ui = ui_send_all(req_rx);

    let out = finish(h, &done_rx);
    let Outcome::Ran(LoopDriverStop::Approve, ledger, channel) = out else {
        panic!("expected Ran(Approve), got {out:?}");
    };
    assert_eq!(channel, fleet.channel_id);
    assert_eq!(ledger.round, 1);
    let seen = ui.join().unwrap();
    assert!(
        seen.contains(&"TurnStarted") && seen.contains(&"TurnEnded"),
        "{seen:?}"
    );
}

#[test]
fn detached_writes_paused_kind_detached_and_sends_no_further_turn() {
    let tmp = home();
    let script = Script::new("accept", &[ISSUE_F1]);
    let (h, req_rx, done_rx, fleet) = fresh(tmp.path(), "review-wrk00003", &script);
    // Send main's turn, then vanish as soon as it starts.
    let ui = std::thread::spawn(move || {
        while let Ok(r) = req_rx.recv() {
            match r {
                DriverReq::Confirm { reply, .. } => {
                    let _ = reply.send(SendAnswer::Send);
                }
                DriverReq::TurnStarted { .. } => return,
                _ => {}
            }
        }
    });

    let out = finish(h, &done_rx);
    ui.join().unwrap();
    let Outcome::Ran(LoopDriverStop::Paused { reason }, ..) = out else {
        panic!("expected a pause, got {out:?}");
    };
    assert_eq!(reason, REVIEW_PAUSE_REASON_DETACHED);
    assert_eq!(
        script.main_calls.load(Ordering::SeqCst),
        1,
        "main's turn completes"
    );
    let all = payloads(tmp.path(), &fleet.channel_id);
    assert_eq!(turn_sents(&all), 1, "the finished reply is kept");
    let p = paused(&all);
    assert_eq!(p.len(), 1);
    assert_eq!(
        (p[0].0, p[0].1.as_str()),
        (PauseKind::Detached, REVIEW_PAUSE_REASON_DETACHED)
    );
    assert!(
        !all.iter()
            .any(|p| matches!(p, ReviewPayload::SessionStopped { .. })),
        "a pause is not an end"
    );
}

#[test]
fn pause_requested_writes_paused_kind_user_after_turn_sent() {
    let tmp = home();
    let script = Script::new("accept", &[APPROVE]);
    let (hold_tx, hold_rx) = channel();
    *script.hold_main.lock().unwrap() = Some(hold_rx);
    let (h, req_rx, done_rx, fleet) = fresh(tmp.path(), "review-wrk00004", &script);
    let flag = h.flags.pause_requested.clone();
    // Esc×1 lands while main's turn is in flight.
    let ui = std::thread::spawn(move || {
        while let Ok(r) = req_rx.recv() {
            match r {
                DriverReq::Confirm { reply, .. } => {
                    let _ = reply.send(SendAnswer::Send);
                }
                DriverReq::TurnStarted { .. } => {
                    flag.store(true, Ordering::Release);
                    let _ = hold_tx.send(());
                }
                _ => {}
            }
        }
    });

    let out = finish(h, &done_rx);
    ui.join().unwrap();
    let Outcome::Ran(LoopDriverStop::Paused { reason }, ..) = out else {
        panic!("expected a pause, got {out:?}");
    };
    assert_eq!(reason, REVIEW_PAUSE_REASON_USER);
    let all = payloads(tmp.path(), &fleet.channel_id);
    assert_eq!(turn_sents(&all), 1, "the in-flight reply is ledgered");
    assert_eq!(paused(&all)[0].0, PauseKind::User);
    assert_eq!(
        script.main_calls.load(Ordering::SeqCst),
        1,
        "no second send"
    );
}

#[test]
fn detached_pause_carries_prompt_wait() {
    let tmp = home();
    let script = Script::new("accept", &[APPROVE]);
    let (h, req_rx, done_rx, fleet) = fresh(tmp.path(), "review-wrk00005", &script);
    // The human sits at the first prompt, then closes MURMUR.
    let ui = std::thread::spawn(move || {
        let r = req_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        drop(r);
        drop(req_rx);
    });

    let out = finish(h, &done_rx);
    ui.join().unwrap();
    assert!(
        matches!(out, Outcome::Ran(LoopDriverStop::Paused { .. }, ..)),
        "{out:?}"
    );
    let p = paused(&payloads(tmp.path(), &fleet.channel_id));
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].0, PauseKind::Detached);
    assert!(
        p[0].2 >= 200,
        "the prompt wait is recorded, got {} ms",
        p[0].2
    );
    assert!(
        p[0].2 < 30_000,
        "and it is a wait, not a clock: {} ms",
        p[0].2
    );
}

/// Reach a session that owes a ruling through the real loop: main rejects
/// F1 in rounds 2 and 3, so F1 escalates at the round-3 seal; the UI answers
/// the ruling prompt with an empty line, which leaves the session paused.
fn escalated(home: &Path, name: &str) {
    let script = Script::new("reject", &[ISSUE_F1, DISPUTE_F1, DISPUTE_F1]);
    let (h, req_rx, done_rx, _) = fresh(home, name, &script);
    let ui = ui_send_all(req_rx);
    let out = finish(h, &done_rx);
    let seen = ui.join().unwrap();
    assert!(
        matches!(out, Outcome::Ran(LoopDriverStop::Paused { .. }, ..)),
        "{out:?} {seen:?}"
    );
    assert!(seen.contains(&"Ruling"), "{seen:?}");
}

#[test]
fn resume_with_pending_ruling_asks_ruling_first() {
    let tmp = home();
    escalated(tmp.path(), "review-wrk00006");

    let script = Script::new("reject", &[]);
    let (h, req_rx, done_rx) = resumed(tmp.path(), "review-wrk00006", &script);
    let first = req_rx.recv().unwrap();
    let DriverReq::Ruling { text, reply, .. } = first else {
        panic!("the first request is the ruling prompt, got {first:?}");
    };
    assert!(text.contains("F1"), "{text}");
    reply.send(String::new()).unwrap();
    while req_rx.recv().is_ok() {}
    assert!(matches!(finish(h, &done_rx), Outcome::LeftPaused));
}

#[test]
fn resume_auto_mode_degrades_with_notice() {
    let tmp = home();
    let home = tmp.path();
    // A session recorded in auto mode whose driver then died: `main` is
    // never sent to (the gate refuses), so only `session_started` exists.
    let fleet = create_session_fleet(home, "review-wrk00007", "main", "reviewer", "t").unwrap();
    let gone = Script::new("accept", &[]);
    let (tx, rx) = channel();
    drop(rx);
    let stopper = super::transport::MurmurTransport::with_io(
        home.to_path_buf(),
        tx,
        Default::default(),
        ["main".into(), "reviewer".into()],
        gone.dial(),
        no_respond(),
    );
    let run = run_review_loop(
        &stopper,
        home,
        &fleet.name,
        &fleet.channel_id,
        "main",
        "reviewer",
        "t",
        Mode::Auto,
        Duration::ZERO,
        limits(),
        &std::time::Instant::now,
    )
    .unwrap();
    assert_eq!(run.1, LoopDriverStop::Stopped);
    assert_eq!(
        prepare_resume(home, &fleet.name).unwrap().ledger.mode,
        Mode::Auto
    );

    let script = Script::new("accept", &[APPROVE]);
    let (h, req_rx, done_rx) = resumed(home, &fleet.name, &script);
    let DriverReq::Show(first) = req_rx.recv().unwrap() else {
        panic!("the degrade notice comes before any prompt");
    };
    assert_eq!(first, REVIEW_AUTO_DEGRADED_NOTICE);
    let ui = ui_send_all(req_rx);
    let out = finish(h, &done_rx);
    ui.join().unwrap();
    assert!(
        matches!(out, Outcome::Ran(LoopDriverStop::Approve, ..)),
        "{out:?}"
    );
}
