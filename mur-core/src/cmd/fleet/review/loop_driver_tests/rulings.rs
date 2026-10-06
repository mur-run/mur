//! P2 Task 7: the live loop waits for a ruling on escalation (P2-§5.1–§5.3)
//! instead of stopping, and applies `/rule` typed at a send prompt.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mur_common::limits::Stuck;

use super::super::constants::{
    REVIEW_BINDING_RULINGS_HEADER, REVIEW_PAUSE_REASON_ESCALATION, RULE_NOT_OPEN_HINT,
    RULING_REGENERATED_BANNER,
};
use super::super::driver::{ReviewTransport, SendAnswer};
use super::super::ledger::{EscalationRecord, Ledger};
use super::super::loop_driver::{LoopDriverStop, run_review_loop};
use super::super::ruling::RulingInput;
use super::super::schema::{
    FindingStatus, Mode, PauseKind, ReviewPayload, Role, RulingDecision, SessionLimits,
};
use super::super::wire::message_text;
use super::{read_payloads, setup_channel};

const FLEET: &str = "review-x";

const ISSUE_F1: &str =
    r#"{"verdict":"revise","findings":[{"severity":"high","issue":"unchecked unwrap"}]}"#;
const DISPUTE_F1: &str =
    r#"{"verdict":"revise","prior":[{"id":"F1","status":"disputed","reason":"still panics"}]}"#;
const KEEP_F1_OPEN: &str = r#"{"verdict":"revise","prior":[{"id":"F1","status":"open"}]}"#;
const RESOLVE_F1_APPROVE: &str =
    r#"{"verdict":"approve","prior":[{"id":"F1","status":"resolved"}]}"#;
const APPROVE: &str = r#"{"verdict":"approve"}"#;

/// Scripted terminal + agents. Main answers every listed finding with
/// `main_answer`; the reviewer pops scripted verdicts; send prompts answer
/// `Send` unless `confirms[(member, nth)]` says otherwise; the ruling
/// prompt pops `asks` (then EOF). `log` records the order of shows,
/// confirms and sends.
struct Scripted {
    home: PathBuf,
    main_answer: &'static str,
    reviewer: RefCell<Vec<String>>,
    confirms: RefCell<HashMap<(&'static str, usize), SendAnswer>>,
    confirm_seen: RefCell<HashMap<String, usize>>,
    asks: RefCell<Vec<String>>,
    ask_count: Cell<usize>,
    /// Advance this clock by `ask_jump` inside every ruling prompt.
    clock: Rc<Cell<Instant>>,
    ask_jump: Duration,
    wait: Cell<Duration>,
    stop_on_ask: bool,
    stop_on_rule_confirm: bool,
    main_texts: RefCell<Vec<String>>,
    log: RefCell<Vec<String>>,
}

impl Scripted {
    fn new(home: &Path, main_answer: &'static str, reviewer: &[&str]) -> Self {
        Self {
            home: home.to_path_buf(),
            main_answer,
            reviewer: RefCell::new(reviewer.iter().rev().map(|s| s.to_string()).collect()),
            confirms: RefCell::default(),
            confirm_seen: RefCell::default(),
            asks: RefCell::default(),
            ask_count: Cell::new(0),
            clock: Rc::new(Cell::new(Instant::now())),
            ask_jump: Duration::ZERO,
            wait: Cell::new(Duration::ZERO),
            stop_on_ask: false,
            stop_on_rule_confirm: false,
            main_texts: RefCell::default(),
            log: RefCell::default(),
        }
    }

    fn asks(self, lines: &[&str]) -> Self {
        *self.asks.borrow_mut() = lines.iter().rev().map(|s| s.to_string()).collect();
        self
    }

    /// The `nth` (1-based) send prompt for `member` answers a `/rule`.
    fn rule_at(self, member: &'static str, nth: usize, decision: RulingDecision) -> Self {
        self.confirms.borrow_mut().insert(
            (member, nth),
            SendAnswer::SendWithRuling(RulingInput {
                finding: "F1".into(),
                decision,
                text: "ruled".into(),
            }),
        );
        self
    }

    fn stop(&self) {
        let path = crate::cmd::fleet::control::stopped_path(&self.home, FLEET);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "stopped\n").unwrap();
    }

    fn shown(&self) -> String {
        self.log
            .borrow()
            .iter()
            .filter_map(|l| l.strip_prefix("show:"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn count(&self, prefix: &str) -> usize {
        self.log
            .borrow()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }
}

impl ReviewTransport for Scripted {
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        let prompt = message_text(params).unwrap_or_default().to_string();
        self.log.borrow_mut().push(format!("send:{member}"));
        if member == "reviewer" {
            return Ok(self
                .reviewer
                .borrow_mut()
                .pop()
                .unwrap_or_else(|| APPROVE.to_string()));
        }
        self.main_texts.borrow_mut().push(prompt.clone());
        let responses: Vec<serde_json::Value> = prompt
            .lines()
            .filter_map(|l| l.strip_prefix("- "))
            .filter_map(|l| l.split_once(" ["))
            .map(|(id, _)| id)
            .filter(|id| id.starts_with('F'))
            .map(|id| serde_json::json!({"id": id, "answer": self.main_answer, "reason": "I disagree"}))
            .collect();
        if responses.is_empty() {
            return Ok("done".into());
        }
        Ok(format!(
            "done\n```json\n{}\n```",
            serde_json::json!({ "responses": responses })
        ))
    }

    fn confirm_send(
        &self,
        member: &str,
        _params: &serde_json::Value,
        _open: &BTreeSet<String>,
    ) -> anyhow::Result<SendAnswer> {
        self.log.borrow_mut().push(format!("confirm:{member}"));
        let mut seen = self.confirm_seen.borrow_mut();
        let n = seen.entry(member.to_string()).or_default();
        *n += 1;
        let key = (if member == "main" { "main" } else { "reviewer" }, *n);
        let answer = self.confirms.borrow_mut().remove(&key);
        if matches!(answer, Some(SendAnswer::SendWithRuling(_))) && self.stop_on_rule_confirm {
            self.stop();
        }
        Ok(answer.unwrap_or(SendAnswer::Send))
    }

    fn ask_ruling(&self, _pending: &EscalationRecord, _ledger: &Ledger) -> anyhow::Result<String> {
        self.ask_count.set(self.ask_count.get() + 1);
        self.clock.set(self.clock.get() + self.ask_jump);
        self.wait.set(self.wait.get() + self.ask_jump);
        if self.stop_on_ask {
            self.stop();
        }
        Ok(self.asks.borrow_mut().pop().unwrap_or_default())
    }

    fn show(&self, text: &str) -> anyhow::Result<()> {
        self.log.borrow_mut().push(format!("show:{text}"));
        Ok(())
    }

    fn take_human_wait(&self) -> Duration {
        self.wait.take()
    }
}

fn run_with(
    t: &Scripted,
    home: &Path,
    channel: &str,
    limits: SessionLimits,
) -> (Ledger, LoopDriverStop) {
    let clock = t.clock.clone();
    run_review_loop(
        t,
        home,
        FLEET,
        channel,
        "main",
        "reviewer",
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        limits,
        &move || clock.get(),
    )
    .unwrap()
}

fn run(t: &Scripted, home: &Path, channel: &str) -> (Ledger, LoopDriverStop) {
    run_with(
        t,
        home,
        channel,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
    )
}

/// Main rejects F1 in rounds 2 and 3 → F1 escalates at the round-3 seal.
fn escalating(home: &Path) -> Scripted {
    Scripted::new(home, "reject", &[ISSUE_F1, DISPUTE_F1, DISPUTE_F1])
}

fn rulings(p: &[ReviewPayload]) -> usize {
    p.iter()
        .filter(|p| matches!(p, ReviewPayload::Ruling { .. }))
        .count()
}

fn index(p: &[ReviewPayload], pred: impl Fn(&ReviewPayload) -> bool) -> usize {
    p.iter().position(pred).expect("payload present")
}

fn is_main_sent(round: u32) -> impl Fn(&ReviewPayload) -> bool {
    move |p| matches!(p, ReviewPayload::TurnSent { round: r, to: Role::Main, .. } if *r == round)
}

fn is_verdict(round: u32) -> impl Fn(&ReviewPayload) -> bool {
    move |p| matches!(p, ReviewPayload::Verdict { round: r, .. } if *r == round)
}

/// Raw channel types, so a payload the review schema no longer parses
/// (the P1 `escalation` event) is still seen.
fn raw_types(home: &Path, channel: &str) -> Vec<String> {
    mur_channel::ChannelService::open(home)
        .unwrap()
        .load_events(channel)
        .unwrap()
        .into_iter()
        .filter_map(|e| e.payload["review"]["type"].as_str().map(str::to_string))
        .collect()
}

/// AC-P2-1 / AC-P2-12 / AC-P2-15 shared assertions: left paused, waiting.
fn assert_left_paused(home: &Path, channel: &str, stop: &LoopDriverStop) {
    assert_eq!(
        *stop,
        LoopDriverStop::Paused {
            reason: REVIEW_PAUSE_REASON_ESCALATION.to_string()
        }
    );
    let types = raw_types(home, channel);
    assert!(
        !types
            .iter()
            .any(|t| t == "session_stopped" || t == "escalation"),
        "{types:?}"
    );
    match read_payloads(home, channel).last() {
        Some(ReviewPayload::Paused { kind, .. }) => assert_eq!(*kind, PauseKind::Escalation),
        other => panic!("expected paused last, got {other:?}"),
    }
}

#[test]
fn escalation_waits_for_ruling() {
    let (tmp, ch) = setup_channel();
    let t = escalating(tmp.path());
    let (ledger, stop) = run(&t, tmp.path(), &ch);
    assert_left_paused(tmp.path(), &ch, &stop);
    assert_eq!(t.ask_count.get(), 1);
    assert_eq!(ledger.pending_ruling().len(), 1, "still owed");
}

#[test]
fn eof_is_q() {
    let (tmp, ch) = setup_channel();
    let t = escalating(tmp.path()).asks(&[""]);
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_left_paused(tmp.path(), &ch, &stop);
}

#[test]
fn q_leaves_paused() {
    let (tmp, ch) = setup_channel();
    let t = escalating(tmp.path()).asks(&["q\n"]);
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_left_paused(tmp.path(), &ch, &stop);
}

#[test]
fn drop_ruling_then_revise_does_not_stop() {
    let (tmp, ch) = setup_channel();
    let t = escalating(tmp.path()).asks(&["/rule drop F1 not worth it\n"]);
    let (ledger, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Approve, "round 4 runs and approves");
    assert_eq!(t.count("send:reviewer"), 4);
    assert!(ledger.pending_ruling().is_empty());
    let p = read_payloads(tmp.path(), &ch);
    assert_eq!(rulings(&p), 1);
    let ruling = index(&p, |p| matches!(p, ReviewPayload::Ruling { .. }));
    assert!(ruling > index(&p, is_verdict(3)) && ruling < index(&p, is_main_sent(4)));
}

#[test]
fn reject_after_fix_blocks_main() {
    let (tmp, ch) = setup_channel();
    let t = escalating(tmp.path()).asks(&["/rule fix F1 do it\n"]);
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Blocked { role: Role::Main });
}

#[test]
fn ruling_wait_trips_neither_limit() {
    let (tmp, ch) = setup_channel();
    let mut t = escalating(tmp.path()).asks(&["/rule drop F1 x\n"]);
    t.ask_jump = Duration::from_secs(15 * 60);
    let limits = SessionLimits::new(
        Duration::from_secs(5 * 60),
        Stuck::After(Duration::from_secs(10 * 60)),
        None,
    );
    let (_, stop) = run_with(&t, tmp.path(), &ch, limits);
    assert_eq!(stop, LoopDriverStop::Approve);
    let p = read_payloads(tmp.path(), &ch);
    match &p[index(&p, is_main_sent(4))] {
        ReviewPayload::TurnSent { human_wait_ms, .. } => {
            assert!(*human_wait_ms >= 15 * 60 * 1000, "{human_wait_ms}")
        }
        _ => unreachable!(),
    }
}

#[test]
fn abandon_stops() {
    let (tmp, ch) = setup_channel();
    let t = escalating(tmp.path()).asks(&["/abandon\n"]);
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Escalation);
    let p = read_payloads(tmp.path(), &ch);
    assert_eq!(rulings(&p), 0);
    assert!(!p.iter().any(|p| matches!(p, ReviewPayload::Paused { .. })));
}

#[test]
fn kill_switch_discards_input() {
    let (tmp, ch) = setup_channel();
    let mut t = escalating(tmp.path()).asks(&["/rule drop F1 x\n"]);
    t.stop_on_ask = true;
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Stopped);
    assert_eq!(rulings(&read_payloads(tmp.path(), &ch)), 0);
}

#[test]
fn invalid_rule_reprompts_without_writing() {
    let (tmp, ch) = setup_channel();
    let t = escalating(tmp.path()).asks(&["/rule drop F9 x\n", "q\n"]);
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_left_paused(tmp.path(), &ch, &stop);
    assert_eq!(t.ask_count.get(), 2, "re-prompted once");
    assert!(
        t.shown()
            .contains(&RULE_NOT_OPEN_HINT.replace("{id}", "F9"))
    );
    let p = read_payloads(tmp.path(), &ch);
    assert_eq!(rulings(&p), 0);
    let paused = p
        .iter()
        .filter(|p| matches!(p, ReviewPayload::Paused { .. }));
    assert_eq!(paused.count(), 1);
}

#[test]
fn main_prompt_rule_applies_before_send() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(tmp.path(), "accept", &[ISSUE_F1, RESOLVE_F1_APPROVE]).rule_at(
        "main",
        2,
        RulingDecision::Fix,
    );
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Approve);
    let p = read_payloads(tmp.path(), &ch);
    assert!(index(&p, |p| matches!(p, ReviewPayload::Ruling { .. })) < index(&p, is_main_sent(2)));
    // Asked once in round 2: the `/rule` line was the consent.
    assert_eq!(t.count("confirm:main"), 2);
    // The banner and then exactly the bytes main received.
    let log = t.log.borrow();
    let at = log.iter().position(|l| l.starts_with("show:")).unwrap();
    let received = t.main_texts.borrow()[1].clone();
    assert_eq!(
        log[at],
        format!("show:{RULING_REGENERATED_BANNER}\n{received}")
    );
    assert_eq!(
        log[at + 1],
        "send:main",
        "nothing between showing and sending"
    );
    assert!(
        received.contains(REVIEW_BINDING_RULINGS_HEADER),
        "the rebuilt message carries the ruling: {received}"
    );
}

#[test]
fn main_prompt_rule_kill_switch() {
    let (tmp, ch) = setup_channel();
    let mut t =
        Scripted::new(tmp.path(), "accept", &[ISSUE_F1]).rule_at("main", 2, RulingDecision::Fix);
    t.stop_on_rule_confirm = true;
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Stopped);
    assert_eq!(rulings(&read_payloads(tmp.path(), &ch)), 0);
    assert_eq!(t.count("send:main"), 1, "round 2 never sent");
}

#[test]
fn reviewer_prompt_rule_lands_after_seal() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(
        tmp.path(),
        "accept",
        &[ISSUE_F1, KEEP_F1_OPEN, RESOLVE_F1_APPROVE],
    )
    .rule_at("reviewer", 2, RulingDecision::Fix);
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(
        stop,
        LoopDriverStop::Approve,
        "an unchanged open set after a ruling is not stuck"
    );
    assert_eq!(
        t.count("send:reviewer"),
        3,
        "the reviewer turn was still sent"
    );
    let p = read_payloads(tmp.path(), &ch);
    let ruling = index(&p, |p| matches!(p, ReviewPayload::Ruling { .. }));
    assert!(index(&p, is_verdict(2)) < ruling && ruling < index(&p, is_main_sent(3)));
}

fn held_case(verdict: &str) -> (tempfile::TempDir, String, Scripted, Ledger, LoopDriverStop) {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(
        tmp.path(),
        "accept",
        &[ISSUE_F1, verdict, RESOLVE_F1_APPROVE],
    )
    .rule_at("reviewer", 2, RulingDecision::Fix);
    let (ledger, stop) = run(&t, tmp.path(), &ch);
    (tmp, ch, t, ledger, stop)
}

#[test]
fn held_rule_revalidated() {
    for status in ["resolved", "withdrawn"] {
        let verdict =
            format!(r#"{{"verdict":"revise","prior":[{{"id":"F1","status":"{status}"}}]}}"#);
        let (tmp, ch, t, _, stop) = held_case(&verdict);
        assert_eq!(stop, LoopDriverStop::Approve, "{status}: round 3 runs");
        assert_eq!(rulings(&read_payloads(tmp.path(), &ch)), 0, "{status}");
        assert!(
            t.shown().contains(&format!(
                "Ruling on F1 discarded: finding is already {status}."
            )),
            "{status}: {}",
            t.shown()
        );
    }
    let (tmp, ch, _, ledger, stop) = held_case(DISPUTE_F1);
    assert_eq!(stop, LoopDriverStop::Approve);
    let p = read_payloads(tmp.path(), &ch);
    let ruling = index(&p, |p| matches!(p, ReviewPayload::Ruling { .. }));
    assert!(ruling < index(&p, is_main_sent(3)));
    assert_eq!(
        ledger.finding("F1").unwrap().ruled,
        Some(RulingDecision::Fix)
    );
    // `fix` folded F1 back to open before round 3 (main saw it open).
    let fold = super::super::ledger::fold(&p[..=ruling]).unwrap();
    assert_eq!(fold.finding("F1").unwrap().status, FindingStatus::Open);
}

#[test]
fn held_rule_dies_with_session() {
    let blocked = r#"{"verdict":"blocked","prior":[{"id":"F1","status":"open"}]}"#;
    for (verdict, expect, word) in [
        (RESOLVE_F1_APPROVE, LoopDriverStop::Approve, "approve"),
        (blocked, LoopDriverStop::ReviewerBlocked, "blocked"),
    ] {
        let (tmp, ch, t, _, stop) = held_case(verdict);
        assert_eq!(stop, expect);
        assert_eq!(rulings(&read_payloads(tmp.path(), &ch)), 0);
        let shown = t.shown();
        assert!(
            shown.contains(&format!(
                "Ruling on F1 discarded: session ended with {word}."
            )),
            "{shown}"
        );
        assert!(!shown.contains("finding is already"), "{shown}");
    }
}
