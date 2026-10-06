//! P3a Task 6: pending notes in the review loop (P3a-§5) and the two-ledger
//! rule — a note flushed at the reviewer's prompt lands in both the sealed
//! ledger and the round's scratch, so live == replay however the turn ends.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use mur_common::limits::Stuck;

use super::super::constants::{REVIEW_BINDING_RULINGS_HEADER, RULING_REGENERATED_BANNER};
use super::super::driver::{ReviewTransport, SendAnswer};
use super::super::ledger::{Ledger, fold_rounds};
use super::super::loop_driver::{LoopDriverStop, Members, continue_review_loop, run_review_loop};
use super::super::ruling::RulingInput;
use super::super::schema::{HumanNote, Mode, ReviewPayload, Role, RulingDecision, SessionLimits};
use super::super::wire::message_text;
use super::{read_payloads, setup_channel, with_accept_all};

const ISSUE_F1: &str =
    r#"{"verdict":"revise","findings":[{"severity":"high","issue":"unchecked unwrap"}]}"#;
const RESOLVE_F1_APPROVE: &str =
    r#"{"verdict":"approve","prior":[{"id":"F1","status":"resolved"}]}"#;
const APPROVE: &str = r#"{"verdict":"approve"}"#;
const KEEP_F1_OPEN: &str = r#"{"verdict":"revise","prior":[{"id":"F1","status":"open"}]}"#;

/// Scripted human + agents. Each member's send prompts pop `answers` in
/// order (then `Send`); `fail` makes that many sends to the member error.
/// `log` records `confirm:<member>:<text>`, `show:<text>`, `send:<member>:<text>`.
#[derive(Default)]
struct Notes {
    answers: RefCell<HashMap<&'static str, Vec<SendAnswer>>>,
    reviewer: RefCell<Vec<String>>,
    fail: RefCell<HashMap<&'static str, usize>>,
    log: RefCell<Vec<String>>,
}

fn key(member: &str) -> &'static str {
    if member == "main" { "main" } else { "reviewer" }
}

impl Notes {
    fn new(reviewer: &[&str]) -> Self {
        let t = Self::default();
        *t.reviewer.borrow_mut() = reviewer.iter().rev().map(|s| s.to_string()).collect();
        t
    }

    fn answers(self, member: &'static str, answers: Vec<SendAnswer>) -> Self {
        let mut a = answers;
        a.reverse();
        self.answers.borrow_mut().insert(member, a);
        self
    }

    fn fail(self, member: &'static str, n: usize) -> Self {
        self.fail.borrow_mut().insert(member, n);
        self
    }

    fn lines(&self, prefix: &str) -> Vec<String> {
        self.log
            .borrow()
            .iter()
            .filter_map(|l| l.strip_prefix(prefix).map(str::to_string))
            .collect()
    }
}

impl ReviewTransport for Notes {
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        let text = message_text(params).unwrap_or_default().to_string();
        if let Some(n) = self.fail.borrow_mut().get_mut(key(member))
            && *n > 0
        {
            *n -= 1;
            anyhow::bail!("transport down");
        }
        self.log.borrow_mut().push(format!("send:{member}:{text}"));
        if member == "main" {
            return Ok(with_accept_all("done", params));
        }
        Ok(self
            .reviewer
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| APPROVE.to_string()))
    }

    fn confirm_send(
        &self,
        member: &str,
        params: &serde_json::Value,
        _open: &BTreeSet<String>,
    ) -> anyhow::Result<SendAnswer> {
        let text = message_text(params).unwrap_or_default();
        self.log
            .borrow_mut()
            .push(format!("confirm:{member}:{text}"));
        Ok(self
            .answers
            .borrow_mut()
            .get_mut(key(member))
            .and_then(Vec::pop)
            .unwrap_or(SendAnswer::Send))
    }

    fn show(&self, text: &str) -> anyhow::Result<()> {
        self.log.borrow_mut().push(format!("show:{text}"));
        Ok(())
    }
}

fn note(text: &str, target: Option<Role>) -> SendAnswer {
    SendAnswer::Note(HumanNote {
        text: text.into(),
        target,
    })
}

fn limits() -> SessionLimits {
    SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None)
}

fn run(t: &Notes, home: &std::path::Path, ch: &str) -> (Ledger, LoopDriverStop) {
    run_review_loop(
        t,
        home,
        "review-x",
        ch,
        "main",
        "reviewer",
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        limits(),
        &Instant::now,
    )
    .unwrap()
}

/// Compact channel shape: `note:<text>`, `sent:<role>:<round>`, `ruling`, …
fn shape(p: &[ReviewPayload]) -> Vec<String> {
    p.iter()
        .filter_map(|p| match p {
            ReviewPayload::HumanNote { text, .. } => Some(format!("note:{text}")),
            ReviewPayload::TurnSent { round, to, .. } => Some(format!("sent:{to:?}:{round}")),
            ReviewPayload::Ruling { .. } => Some("ruling".into()),
            ReviewPayload::Paused { .. } => Some("paused".into()),
            _ => None,
        })
        .collect()
}

fn assert_replay(home: &std::path::Path, ch: &str, live: &Ledger) {
    let replay = fold_rounds(&read_payloads(home, ch)).unwrap();
    assert_eq!(live, &replay, "live ledger must equal replay (AC-P3a-14)");
}

/// The live ledger of a paused session, as replay sees it: the driver writes
/// `paused` straight to the channel, so only that flag differs.
fn assert_replay_paused(home: &std::path::Path, ch: &str, live: &Ledger) {
    let mut live = live.clone();
    live.paused = true;
    assert_replay(home, ch, &live);
}

/// AC-P3a-1, 2, 14.
#[test]
fn broadcast_note_at_main_prompt() {
    let (tmp, ch) = setup_channel();
    let t = Notes::new(&[ISSUE_F1, RESOLVE_F1_APPROVE]).answers("main", vec![note("NOTE-X", None)]);
    let (live, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Approve);
    let p = read_payloads(tmp.path(), &ch);
    let s = shape(&p);
    assert_eq!(&s[..2], ["note:NOTE-X", "sent:Main:1"]);
    let confirms = t.lines("confirm:main:");
    let sends = t.lines("send:main:");
    assert!(confirms[1].contains("NOTE-X"), "reprint shows X");
    assert_eq!(sends[0], confirms[1], "sent bytes == reprinted bytes");
    assert!(t.lines("send:reviewer:")[0].contains("NOTE-X"), "AC-P3a-2");
    assert!(!sends[1].contains("NOTE-X"), "main's TurnSent cleared it");
    assert_replay(tmp.path(), &ch, &live);
}

/// AC-P3a-3, 14.
#[test]
fn targeted_note_skips_main() {
    let (tmp, ch) = setup_channel();
    let t = Notes::new(&[APPROVE]).answers("main", vec![note("NOTE-Y", Some(Role::Reviewer))]);
    let (live, _) = run(&t, tmp.path(), &ch);
    assert!(!t.lines("confirm:main:")[1].contains("NOTE-Y"));
    assert!(!t.lines("send:main:")[0].contains("NOTE-Y"));
    let p = read_payloads(tmp.path(), &ch);
    assert!(p.iter().any(|p| matches!(p,
        ReviewPayload::HumanNote { text, target: Some(Role::Reviewer) } if text == "NOTE-Y")));
    assert!(t.lines("send:reviewer:")[0].contains("NOTE-Y"));
    assert_replay(tmp.path(), &ch, &live);
}

/// AC-P3a-6.
#[test]
fn notes_flush_in_order() {
    let (tmp, ch) = setup_channel();
    let t =
        Notes::new(&[APPROVE]).answers("main", vec![note("NOTE-A", None), note("NOTE-B", None)]);
    run(&t, tmp.path(), &ch);
    let c = t.lines("confirm:main:");
    assert!(c[1].contains("NOTE-A") && !c[1].contains("- NOTE-B"));
    assert!(c[2].contains("NOTE-A") && c[2].contains("- NOTE-B"));
    let s = shape(&read_payloads(tmp.path(), &ch));
    assert_eq!(&s[..3], ["note:NOTE-A", "note:NOTE-B", "sent:Main:1"]);
}

/// AC-P3a-7.
#[test]
fn stop_discards_notes() {
    let (tmp, ch) = setup_channel();
    let t = Notes::new(&[]).answers("main", vec![note("NOTE-A", None), SendAnswer::Stop]);
    let (_, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Stopped);
    assert!(shape(&read_payloads(tmp.path(), &ch)).is_empty());
}

/// AC-P3a-8 (rev 5): a note then a `/rule` at the reviewer's (non-boundary)
/// prompt — the note is flushed before that `turn_sent` and carried by the
/// message; the ruling is held per P2-§5.3 and lands only after the round's
/// seal, so it is neither on the channel before that `turn_sent` nor in it.
#[test]
fn note_then_rule_at_reviewer_prompt_flushes() {
    let (tmp, ch) = setup_channel();
    let rule = SendAnswer::SendWithRuling(RulingInput {
        finding: "F1".into(),
        decision: RulingDecision::Fix,
        text: "RULING-TXT".into(),
    });
    let t = Notes::new(&[ISSUE_F1, KEEP_F1_OPEN, RESOLVE_F1_APPROVE]).answers(
        "reviewer",
        vec![SendAnswer::Send, note("NOTE-A", None), rule],
    );
    let (live, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Approve);
    let p = read_payloads(tmp.path(), &ch);
    let s = shape(&p);
    let at = s.iter().position(|x| x == "note:NOTE-A").unwrap();
    let sent = at + 1;
    assert_eq!(s[sent], "sent:Reviewer:2");
    // Held, not written before the reviewer's send.
    assert!(!s[..sent].contains(&"ruling".to_string()), "{s:?}");
    // Lands after the round-2 seal, before main's round-3 `turn_sent`.
    let idx = |f: &dyn Fn(&ReviewPayload) -> bool| p.iter().position(f).unwrap();
    let verdict2 = idx(&|x| matches!(x, ReviewPayload::Verdict { round: 2, .. }));
    let ruling = idx(&|x| matches!(x, ReviewPayload::Ruling { .. }));
    let main3 = idx(&|x| {
        matches!(
            x,
            ReviewPayload::TurnSent {
                round: 3,
                to: Role::Main,
                ..
            }
        )
    });
    assert!(verdict2 < ruling && ruling < main3, "{s:?}");
    // The reviewer's round-2 message carries the note, not the ruling.
    let msg = &t.lines("send:reviewer:")[1];
    assert!(msg.contains("NOTE-A"));
    assert!(!msg.contains("RULING-TXT"), "{msg}");
    assert!(!msg.contains(REVIEW_BINDING_RULINGS_HEADER), "{msg}");
    assert_replay(tmp.path(), &ch, &live);
}

/// AC-P3a-9, 14: boundary `/rule` keeps the note; no further prompt.
#[test]
fn note_then_boundary_rule() {
    let (tmp, ch) = setup_channel();
    let rule = SendAnswer::SendWithRuling(RulingInput {
        finding: "F1".into(),
        decision: RulingDecision::Fix,
        text: "ruled".into(),
    });
    let t = Notes::new(&[ISSUE_F1, RESOLVE_F1_APPROVE])
        .answers("main", vec![SendAnswer::Send, note("NOTE-A", None), rule]);
    let (live, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Approve);
    let s = shape(&read_payloads(tmp.path(), &ch));
    let at = s.iter().position(|x| x == "ruling").unwrap();
    assert_eq!(&s[at..at + 3], ["ruling", "note:NOTE-A", "sent:Main:2"]);
    // P2's `main_prompt_rule_applies_before_send` asks main twice too.
    assert_eq!(
        t.lines("confirm:main:").len(),
        3,
        "2 prompts + the note reprint"
    );
    let received = t.lines("send:main:")[1].clone();
    assert!(received.contains("NOTE-A") && received.contains(REVIEW_BINDING_RULINGS_HEADER));
    let log = t.log.borrow();
    let shown = log.iter().position(|l| l.starts_with("show:")).unwrap();
    assert_eq!(
        log[shown],
        format!("show:{RULING_REGENERATED_BANNER}\n{received}")
    );
    assert_eq!(
        log[shown + 1],
        format!("send:main:{received}"),
        "no prompt between"
    );
    drop(log);
    assert_replay(tmp.path(), &ch, &live);
}

/// AC-P3a-10, 14: a note at a validation resend prompt.
#[test]
fn note_at_resend_prompt() {
    let (tmp, ch) = setup_channel();
    let t = Notes::new(&["not json", APPROVE])
        .answers("reviewer", vec![SendAnswer::Send, note("NOTE-A", None)]);
    let (live, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Approve);
    let c = t.lines("confirm:reviewer:");
    assert!(c[2].contains("- NOTE-A") && c[2].contains("could not be accepted"));
    assert_eq!(t.lines("send:reviewer:")[1], c[2]);
    assert_replay(tmp.path(), &ch, &live);
}

/// AC-P3a-11, 14: at-least-once, and resume sends the note again.
#[test]
fn note_survives_transport_pause() {
    let (tmp, ch) = setup_channel();
    let t = Notes::new(&[])
        .answers("main", vec![note("NOTE-A", None)])
        .fail("main", 2);
    let (live, stop) = run(&t, tmp.path(), &ch);
    assert!(matches!(stop, LoopDriverStop::Paused { .. }));
    let p = read_payloads(tmp.path(), &ch);
    assert_eq!(shape(&p), ["note:NOTE-A", "paused"]);
    assert_replay_paused(tmp.path(), &ch, &live);

    let resumed = fold_rounds(&p).unwrap();
    let again = Notes::new(&[APPROVE]);
    let members = Members {
        fleet_name: "review-x",
        channel_id: &ch,
        main: "main",
        reviewer: "reviewer",
        task: "task",
    };
    continue_review_loop(
        &again,
        tmp.path(),
        &members,
        resumed,
        1,
        limits(),
        Duration::ZERO,
        Duration::ZERO,
        &Instant::now,
    )
    .unwrap();
    assert!(again.lines("send:main:")[0].contains("- NOTE-A"));
}

/// Two-ledger rule, `ledger` half: the session stops mid-reviewer-turn and
/// returns `ledger`, which must hold the reviewer-side note.
#[test]
fn reviewer_side_note_then_stop_replays_equal_to_live() {
    let (tmp, ch) = setup_channel();
    let t = Notes::new(&[])
        .answers("reviewer", vec![note("NOTE-A", None)])
        .fail("reviewer", 2);
    let (live, stop) = run(&t, tmp.path(), &ch);
    assert!(matches!(stop, LoopDriverStop::Paused { .. }));
    assert_eq!(live.unseen_notes(Role::Reviewer).len(), 1);
    assert_replay_paused(tmp.path(), &ch, &live);
}

/// Two-ledger rule, `round_ledger` half: the verdict seal adopts the round
/// ledger, which must hold the reviewer-side note.
#[test]
fn reviewer_side_note_survives_verdict_seal() {
    let (tmp, ch) = setup_channel();
    let t = Notes::new(&[APPROVE]).answers("reviewer", vec![note("NOTE-A", None)]);
    let (live, stop) = run(&t, tmp.path(), &ch);
    assert_eq!(stop, LoopDriverStop::Approve);
    // The reviewer's `turn_sent` cleared its own copy; main's is still unseen.
    assert_eq!(live.unseen_notes(Role::Main).len(), 1);
    assert_replay(tmp.path(), &ch, &live);
}
