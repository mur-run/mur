//! P2 Task 10: `review-resume` of a session that owes a ruling (P2-§6).
//! Fixtures reach the escalation through the real loop: main rejects F1 in
//! rounds 2 and 3, so F1 escalates at the round-3 seal.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;

use mur_common::channel::EventKind;
use mur_common::limits::Stuck;

use super::{ResumeEnd, prepare_resume, settle_then_resume};
use crate::cmd::fleet::review::constants::REVIEW_PAUSE_REASON_CRASHED;
use crate::cmd::fleet::review::driver::ReviewTransport;
use crate::cmd::fleet::review::ledger::{EscalationRecord, Ledger};
use crate::cmd::fleet::review::loop_driver::LoopDriverStop;
use crate::cmd::fleet::review::schema::{
    NoteClassification, PauseKind, ReviewPayload, Role, SessionLimits, classify_note_payload,
};
use crate::cmd::fleet::review::session::{create_session_fleet, run_session};
use crate::cmd::fleet::review::wire::message_text;
use crate::cmd::fleet::{control, store};

const ISSUE_F1: &str =
    r#"{"verdict":"revise","findings":[{"severity":"high","issue":"unchecked unwrap"}]}"#;
const DISPUTE_F1: &str =
    r#"{"verdict":"revise","prior":[{"id":"F1","status":"disputed","reason":"still panics"}]}"#;
const RESOLVE_F1_APPROVE: &str =
    r#"{"verdict":"approve","prior":[{"id":"F1","status":"resolved"}]}"#;

/// Main answers every listed finding with `answer`; the reviewer pops a
/// script; the ruling prompt pops `asks` (then EOF), optionally setting the
/// kill-switch, sleeping, or panicking (a SIGKILL stand-in) first.
struct Scripted<'a> {
    home: &'a Path,
    fleet: String,
    answer: &'static str,
    reviewer: RefCell<Vec<&'static str>>,
    asks: RefCell<Vec<&'static str>>,
    ask_count: Cell<usize>,
    ask_sleep: Duration,
    stop_on_ask: bool,
    panic_on_ask: bool,
    main_sent: Cell<usize>,
    /// Time slept at the ruling prompt, reported as human wait the way
    /// `TerminalGate::ask_ruling` times its prompt.
    waited: Cell<Duration>,
}

impl<'a> Scripted<'a> {
    fn new(home: &'a Path, fleet: &str, answer: &'static str, reviewer: &[&'static str]) -> Self {
        Self {
            home,
            fleet: fleet.to_string(),
            answer,
            reviewer: RefCell::new(reviewer.iter().rev().copied().collect()),
            asks: RefCell::default(),
            ask_count: Cell::new(0),
            ask_sleep: Duration::ZERO,
            stop_on_ask: false,
            panic_on_ask: false,
            main_sent: Cell::new(0),
            waited: Cell::new(Duration::ZERO),
        }
    }

    fn asks(self, lines: &[&'static str]) -> Self {
        *self.asks.borrow_mut() = lines.iter().rev().copied().collect();
        self
    }
}

impl ReviewTransport for Scripted<'_> {
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        if member == "reviewer" {
            return Ok(self
                .reviewer
                .borrow_mut()
                .pop()
                .expect("reviewer script")
                .to_string());
        }
        self.main_sent.set(self.main_sent.get() + 1);
        let responses: Vec<serde_json::Value> = message_text(params)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("- "))
            .filter_map(|l| l.split_once(" ["))
            .map(|(id, _)| id)
            .filter(|id| id.starts_with('F'))
            .map(|id| serde_json::json!({"id": id, "answer": self.answer, "reason": "r"}))
            .collect();
        Ok(format!(
            "done\n```json\n{}\n```",
            serde_json::json!({ "responses": responses })
        ))
    }

    fn take_human_wait(&self) -> Duration {
        self.waited.take()
    }

    fn ask_ruling(&self, _p: &EscalationRecord, _l: &Ledger) -> anyhow::Result<String> {
        self.ask_count.set(self.ask_count.get() + 1);
        assert!(!self.panic_on_ask, "driver killed at the ruling prompt");
        std::thread::sleep(self.ask_sleep);
        self.waited.set(self.waited.get() + self.ask_sleep);
        if self.stop_on_ask {
            let path = control::stopped_path(self.home, &self.fleet);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "stopped\n").unwrap();
        }
        Ok(self.asks.borrow_mut().pop().unwrap_or_default().to_string())
    }
}

fn limits() -> SessionLimits {
    SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None)
}

fn payloads(home: &Path, name: &str) -> Vec<ReviewPayload> {
    mur_channel::ChannelService::open(home)
        .unwrap()
        .load_events(&format!("fleet-{name}"))
        .unwrap()
        .into_iter()
        .filter(|ev| ev.kind == EventKind::Note)
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some(env.payload),
            _ => None,
        })
        .collect()
}

fn events_bytes(home: &Path, name: &str) -> Vec<u8> {
    let svc = mur_channel::ChannelService::open(home).unwrap();
    std::fs::read(svc.store().events_path(&format!("fleet-{name}"))).unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Fixture {
    /// The human typed `q` at the live ruling prompt: `paused{escalation}`.
    Paused,
    /// The driver died at the live ruling prompt: no `paused` at all.
    Crashed,
}

/// Run a session up to the round-3 escalation and leave it `fixture`.
fn escalated(fixture: Fixture, name: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let fleet = create_session_fleet(home, name, "main", "reviewer", "t").unwrap();
    let mut t = Scripted::new(home, name, "reject", &[ISSUE_F1, DISPUTE_F1, DISPUTE_F1]);
    match fixture {
        Fixture::Paused => {
            let (_, stop) = run_session(&t, home, &fleet, "t", limits(), Duration::ZERO).unwrap();
            assert!(matches!(stop, LoopDriverStop::Paused { .. }), "{stop:?}");
        }
        Fixture::Crashed => {
            t.panic_on_ask = true;
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_session(&t, home, &fleet, "t", limits(), Duration::ZERO)
            }));
            assert!(run.is_err(), "the scripted kill must fire");
            assert!(
                !payloads(home, name)
                    .iter()
                    .any(|p| matches!(p, ReviewPayload::Paused { .. })),
                "a killed driver leaves no paused"
            );
        }
    }
    tmp
}

fn is_ruling(p: &ReviewPayload) -> bool {
    matches!(p, ReviewPayload::Ruling { .. })
}

fn pos(all: &[ReviewPayload], pred: impl Fn(&ReviewPayload) -> bool) -> usize {
    all.iter().position(pred).expect("payload present")
}

/// AC-P2-5: a session killed at the ruling prompt resumes AT the ruling
/// prompt, not the crashed continue path, and its `paused{crashed}` is
/// written before any ruling.
#[test]
fn sigkill_while_awaiting_resumes_at_ruling_prompt() {
    let name = "review-rsrl0001";
    let tmp = escalated(Fixture::Crashed, name);
    let home = tmp.path();
    let r = prepare_resume(home, name).unwrap();
    assert!(r.crashed);
    assert_eq!(
        r.ledger.pending_ruling().len(),
        1,
        "the crash cannot skip it"
    );

    let t = Scripted::new(home, name, "accept", &[RESOLVE_F1_APPROVE])
        .asks(&["/rule fix F1 bounds-check it\n"]);
    let end = settle_then_resume(&t, home, r, Duration::ZERO).unwrap();
    assert!(
        matches!(end, ResumeEnd::Ran(_, LoopDriverStop::Approve)),
        "{end:?}"
    );
    assert_eq!(t.ask_count.get(), 1);

    let all = payloads(home, name);
    let crashed = pos(
        &all,
        |p| matches!(p, ReviewPayload::Paused { reason, .. } if reason == REVIEW_PAUSE_REASON_CRASHED),
    );
    assert!(crashed < pos(&all, is_ruling));
}

/// AC-P2-6: paused awaiting a ruling → resume → `/rule` → `resumed` → the
/// next round, in that channel order.
#[test]
fn paused_escalation_rule_resumed_next_round() {
    let name = "review-rsrl0002";
    let tmp = escalated(Fixture::Paused, name);
    let home = tmp.path();
    let r = prepare_resume(home, name).unwrap();
    assert!(!r.crashed);
    let t = Scripted::new(home, name, "accept", &[RESOLVE_F1_APPROVE])
        .asks(&["/rule fix F1 bounds-check it\n"]);
    let end = settle_then_resume(&t, home, r, Duration::ZERO).unwrap();
    assert!(
        matches!(end, ResumeEnd::Ran(_, LoopDriverStop::Approve)),
        "{end:?}"
    );

    let all = payloads(home, name);
    let paused = pos(&all, |p| {
        matches!(
            p,
            ReviewPayload::Paused {
                kind: PauseKind::Escalation,
                ..
            }
        )
    });
    let ruling = pos(&all, is_ruling);
    let resumed = pos(&all, |p| matches!(p, ReviewPayload::Resumed { .. }));
    let main4 = pos(&all, |p| {
        matches!(
            p,
            ReviewPayload::TurnSent {
                round: 4,
                to: Role::Main,
                ..
            }
        )
    });
    assert!(
        paused < ruling && ruling < resumed && resumed < main4,
        "{all:?}"
    );
}

/// AC-P2-16: `/abandon` on the resume path ends the session, paused or
/// crashed alike.
#[test]
fn abandon_from_resume() {
    for (fixture, name) in [
        (Fixture::Paused, "review-rsrl0003"),
        (Fixture::Crashed, "review-rsrl0004"),
    ] {
        let tmp = escalated(fixture, name);
        let home = tmp.path();
        let r = prepare_resume(home, name).unwrap();
        let t = Scripted::new(home, name, "accept", &[]).asks(&["/abandon\n"]);
        let end = settle_then_resume(&t, home, r, Duration::ZERO).unwrap();
        assert!(
            matches!(end, ResumeEnd::Ran(_, LoopDriverStop::Escalation)),
            "{fixture:?}: {end:?}"
        );
        assert_eq!(t.main_sent.get(), 0, "{fixture:?}: nothing sent");
        assert!(!store::fleet_path(home, name).exists(), "{fixture:?}");
        match payloads(home, name).last() {
            Some(ReviewPayload::SessionStopped { reason, .. }) => {
                assert_eq!(reason, "escalation", "{fixture:?}")
            }
            other => panic!("{fixture:?}: last must be session_stopped, got {other:?}"),
        }
    }
}

/// AC-P2-17: `q` at the resume ruling prompt writes nothing after the
/// prompt; for a crashed session the only new event is the pre-prompt
/// `paused`. The lock is released either way.
#[test]
fn q_on_paused_resume_writes_nothing() {
    for (fixture, name) in [
        (Fixture::Paused, "review-rsrl0005"),
        (Fixture::Crashed, "review-rsrl0006"),
    ] {
        let tmp = escalated(fixture, name);
        let home = tmp.path();
        let before = payloads(home, name).len();
        let r = prepare_resume(home, name).unwrap();
        let at_prompt = events_bytes(home, name);
        let t = Scripted::new(home, name, "accept", &[]).asks(&["q\n"]);
        let end = settle_then_resume(&t, home, r, Duration::ZERO).unwrap();
        assert!(matches!(end, ResumeEnd::LeftPaused), "{fixture:?}: {end:?}");
        assert_eq!(
            events_bytes(home, name),
            at_prompt,
            "{fixture:?}: tail unchanged"
        );

        let after = payloads(home, name);
        match fixture {
            Fixture::Paused => assert_eq!(after.len(), before, "nothing at all"),
            Fixture::Crashed => {
                assert_eq!(after.len(), before + 1, "only the pre-prompt paused");
                assert!(matches!(
                    after.last(),
                    Some(ReviewPayload::Paused { reason, .. }) if reason == REVIEW_PAUSE_REASON_CRASHED
                ));
            }
        }
        let again = prepare_resume(home, name).expect("lock released, still resumable");
        assert!(!again.crashed, "{fixture:?}: now an ordinary pause");
    }
}

/// The kill-switch set while the human sits at the resume ruling prompt
/// wins over the line they typed: P1 kill-switch stop, no `ruling`.
#[test]
fn kill_switch_at_resume_prompt() {
    let name = "review-rsrl0007";
    let tmp = escalated(Fixture::Paused, name);
    let home = tmp.path();
    let r = prepare_resume(home, name).unwrap();
    let mut t = Scripted::new(home, name, "accept", &[]).asks(&["/rule drop F1 x\n"]);
    t.stop_on_ask = true;
    let end = settle_then_resume(&t, home, r, Duration::ZERO).unwrap();
    assert!(
        matches!(end, ResumeEnd::Ran(_, LoopDriverStop::Stopped)),
        "{end:?}"
    );
    let all = payloads(home, name);
    assert!(!all.iter().any(is_ruling), "{all:?}");
    assert!(matches!(
        all.last(),
        Some(ReviewPayload::SessionStopped { reason, .. }) if reason == "stopped"
    ));
}

/// D2 regression: the human's wait at the resume ruling prompt of a
/// crashed session is not execution time. Had `paused{crashed}` been
/// written after the prompt, the crashed segment would run through the
/// wait. Runner speed varies (slow CI runners spend well over the wait on
/// the scripted run itself), so the bound is relative to wall-clock: with
/// the wait excluded, active time is at most `wall - WAIT`; with it leaked,
/// active time tracks `wall`.
#[test]
fn active_time_excludes_ruling_wait_on_crashed() {
    const WAIT: Duration = Duration::from_millis(600);
    let name = "review-rsrl0008";
    let started = std::time::Instant::now();
    let tmp = escalated(Fixture::Crashed, name);
    let home = tmp.path();
    let r = prepare_resume(home, name).unwrap();
    let mut t = Scripted::new(home, name, "accept", &[RESOLVE_F1_APPROVE])
        .asks(&["/rule fix F1 bounds-check it\n"]);
    t.ask_sleep = WAIT;
    let end = settle_then_resume(&t, home, r, Duration::ZERO).unwrap();
    let wall = started.elapsed();
    assert!(
        matches!(end, ResumeEnd::Ran(_, LoopDriverStop::Approve)),
        "{end:?}"
    );

    let events = mur_channel::ChannelService::open(home)
        .unwrap()
        .load_events(&format!("fleet-{name}"))
        .unwrap();
    let active = super::active_time(&super::review_events(&events));
    assert!(
        active + WAIT / 2 < wall,
        "wait leaked into active time: active {active:?}, wall {wall:?}"
    );
}

/// QA P1, live path: the human waits at the live ruling prompt, then `q`.
/// That wait ended in `paused`, never in a `turn_sent`, so replay must still
/// take it out of execution time (P2-§5.1: "human-input wait ... excluded
/// from `deadline`"). Bound relative to wall-clock, as above.
#[test]
fn live_ruling_wait_then_q_is_not_execution_time_on_resume() {
    const WAIT: Duration = Duration::from_millis(600);
    let name = "review-rsrl0009";
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let fleet = create_session_fleet(home, name, "main", "reviewer", "t").unwrap();
    let started = std::time::Instant::now();
    let mut t =
        Scripted::new(home, name, "reject", &[ISSUE_F1, DISPUTE_F1, DISPUTE_F1]).asks(&["q\n"]);
    t.ask_sleep = WAIT;
    let (_, stop) = run_session(&t, home, &fleet, "t", limits(), Duration::ZERO).unwrap();
    let wall = started.elapsed();
    assert!(matches!(stop, LoopDriverStop::Paused { .. }), "{stop:?}");

    let r = prepare_resume(home, name).unwrap();
    assert!(
        r.active + WAIT / 2 < wall,
        "ruling wait leaked into active time: active {:?}, wall {wall:?}",
        r.active
    );
}

/// QA P1, crashed path: `paused{crashed}` is written at resume time, so the
/// offline gap between the crash and that resume must not become execution
/// time on a second resume.
#[test]
fn crashed_q_then_second_resume_excludes_offline_gap() {
    const OFFLINE: Duration = Duration::from_millis(600);
    let name = "review-rsrl0010";
    let tmp = escalated(Fixture::Crashed, name);
    let home = tmp.path();
    std::thread::sleep(OFFLINE);
    let first = prepare_resume(home, name).unwrap();
    let first_active = first.active;
    let t = Scripted::new(home, name, "accept", &[]).asks(&["q\n"]);
    let end = settle_then_resume(&t, home, first, Duration::ZERO).unwrap();
    assert!(matches!(end, ResumeEnd::LeftPaused), "{end:?}");

    let second = prepare_resume(home, name).unwrap();
    assert!(
        second.active < first_active + OFFLINE / 2,
        "offline gap leaked: first {first_active:?}, second {:?}",
        second.active
    );
}
