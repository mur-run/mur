//! Resuming after a MURMUR pause or abort (P3b-§8.7; AC-P3b-27, AC-P3b-33).
//! The UI side is a scripted thread that does what Esc ×1 / Esc ×2 do to
//! the worker: set `pause_requested`, or win the turn's `TurnCell`.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use super::super::constants::{REVIEW_PAUSE_REASON_ABORTED, REVIEW_RESUME_RESTARTS_ROUND};
use super::super::driver::SendAnswer;
use super::super::ledger::fold_rounds;
use super::super::loop_driver::LoopDriverStop;
use super::super::resume::prepare_resume;
use super::super::schema::{PauseKind, ReviewPayload, Role};
use super::super::session::render_resume_summary;
use super::bridge::{DriverReq, Outcome};
use super::transport::DialFn;
use super::worker_tests::{
    APPROVE, ISSUE_F1, Script, finish, fresh, home, paused, payloads, resumed, task,
};

/// The reviewer's resolving approve for F1 after the restarted round.
const APPROVE_F1: &str = r#"{"verdict":"approve","prior":[{"id":"F1","status":"resolved"}]}"#;

fn sends(all: &[ReviewPayload], round: u32, to: Role) -> usize {
    all.iter()
        .filter(|p| matches!(p, ReviewPayload::TurnSent { round: r, to: t, .. } if *r == round && *t == to))
        .count()
}

fn verdicts(all: &[ReviewPayload], round: u32) -> usize {
    all.iter()
        .filter(|p| matches!(p, ReviewPayload::Verdict { round: r, .. } if *r == round))
        .count()
}

/// AC-P3b-27 (P1 AC20 as amended): Esc ×1 during main's round-1 turn pauses
/// after it; `/review resume` continues at round 1 and the session ends
/// there, the ledger's round unchanged.
#[test]
fn esc_once_pause_then_resume_continues_at_the_same_round() {
    let tmp = home();
    let script = Script::new("accept", &[]);
    let (hold_tx, hold_rx) = channel();
    *script.hold_main.lock().unwrap() = Some(hold_rx);
    let (h, req_rx, done_rx, fleet) = fresh(tmp.path(), "review-wrr00001", &script);
    let flag = h.flags.pause_requested.clone();
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
    assert!(
        matches!(out, Outcome::Ran(LoopDriverStop::Paused { .. }, ..)),
        "{out:?}"
    );
    assert_eq!(
        paused(&payloads(tmp.path(), &fleet.channel_id))[0].0,
        PauseKind::User
    );

    let r = prepare_resume(tmp.path(), &fleet.name).unwrap();
    assert_eq!(r.round, 1, "resumes at the paused round");
    drop(r);
    let again = Script::new("accept", &[APPROVE]);
    let (h, req_rx, done_rx) = resumed(tmp.path(), &fleet.name, &again);
    let ui = super::worker_tests::ui_send_all(req_rx);
    let out = finish(h, &done_rx);
    ui.join().unwrap();
    let Outcome::Ran(LoopDriverStop::Approve, ledger, _) = out else {
        panic!("expected Approve after resume, got {out:?}");
    };
    assert_eq!(ledger.round, 1, "the same round, sealed once");
    assert_eq!(verdicts(&payloads(tmp.path(), &fleet.channel_id), 1), 1);
}

/// Scripted members that record who was sent to, holding the reviewer's
/// `hold_at`-th turn until the UI has aborted it.
fn recording_dial(calls: Arc<Mutex<Vec<String>>>, hold_at: usize, hold: Receiver<()>) -> DialFn {
    let script = Script::new("accept", &[ISSUE_F1]);
    let inner = script.dial();
    let hold = Mutex::new(Some(hold));
    Arc::new(move |home, member, params, on_delta, on_hitl| {
        let n = {
            let mut c = calls.lock().unwrap();
            c.push(member.to_string());
            c.iter().filter(|m| *m == "reviewer").count()
        };
        if member == "reviewer" && n == hold_at {
            if let Some(rx) = hold.lock().unwrap().take() {
                let _ = rx.recv();
            }
            on_delta("x", false, "T-R");
            return Ok(task(APPROVE));
        }
        inner(home, member, params, on_delta, on_hitl)
    })
}

/// AC-P3b-33 / §8.7: Esc ×2 on the reviewer's round-2 turn pauses the
/// session; resuming says round 2 restarts from main, main is sent round 2
/// a second time, and the channel holds exactly one sealed round 2.
#[test]
fn abort_reviewer_then_resume_resends_main_once_per_round() {
    let tmp = home();
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let (go_tx, go_rx) = channel();
    let dial = recording_dial(calls.clone(), 2, go_rx);
    let (h, req_rx, done_rx, fleet) =
        super::worker_tests::fresh_with_dial(tmp.path(), "review-wrr00002", dial);
    let ui = std::thread::spawn(move || {
        let mut reviewer_turns = 0;
        while let Ok(r) = req_rx.recv() {
            match r {
                DriverReq::Confirm { reply, .. } => {
                    let _ = reply.send(SendAnswer::Send);
                }
                DriverReq::TurnStarted { member, turn, .. } if member == "reviewer" => {
                    reviewer_turns += 1;
                    if reviewer_turns == 2 {
                        assert!(turn.abort(), "Esc ×2 wins while the turn is in flight");
                        let _ = go_tx.send(());
                    }
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
    assert_eq!(reason, REVIEW_PAUSE_REASON_ABORTED);
    assert_eq!(
        *calls.lock().unwrap(),
        ["main", "reviewer", "main", "reviewer"]
    );

    let r = prepare_resume(tmp.path(), &fleet.name).unwrap();
    assert_eq!(r.round, 2);
    assert!(
        render_resume_summary(&r).contains(&REVIEW_RESUME_RESTARTS_ROUND.replace("{n}", "2")),
        "{}",
        render_resume_summary(&r)
    );
    drop(r);
    let again = Script::new("accept", &[APPROVE_F1]);
    let (h, req_rx, done_rx) = resumed(tmp.path(), &fleet.name, &again);
    let ui = super::worker_tests::ui_send_all(req_rx);
    let out = finish(h, &done_rx);
    ui.join().unwrap();
    assert!(
        matches!(out, Outcome::Ran(LoopDriverStop::Approve, ..)),
        "{out:?}"
    );

    let all = payloads(tmp.path(), &fleet.channel_id);
    assert_eq!(
        sends(&all, 2, Role::Main),
        2,
        "main's round 2 is sent again"
    );
    assert_eq!(again.main_calls.load(Ordering::SeqCst), 1);
    assert_eq!(verdicts(&all, 2), 1, "one sealed round 2");
    assert_eq!(fold_rounds(&all).unwrap().round, 2);
}
