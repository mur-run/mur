//! P3a Task 5: the note queue in the send gate (`run_turn_with_retry`'s
//! generator + `on_consented`). Scripted answers, every shown and sent
//! `params` recorded, so "sent == last shown" (N5) is checked directly.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use super::driver::{
    FlushFailed, RetryOutcome, ReviewTransport, SendAnswer, SendGate, run_turn_with_retry,
};
use super::driver_tests::setup_channel;
use super::ruling::RulingInput;
use super::schema::{
    HumanNote, NoteClassification, PauseKind, ReviewPayload, Role, RulingDecision,
    classify_note_payload,
};

/// Scripted gate: answers in order (then `Send`), replies in order (then
/// `Ok("")`); records every `params` shown at the prompt and every one sent.
#[derive(Default)]
struct Scripted {
    answers: Mutex<Vec<SendAnswer>>,
    replies: Mutex<Vec<anyhow::Result<String>>>,
    shown: Mutex<Vec<serde_json::Value>>,
    sent: Mutex<Vec<serde_json::Value>>,
    printed: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(answers: Vec<SendAnswer>, replies: Vec<anyhow::Result<String>>) -> Self {
        let (mut a, mut r) = (answers, replies);
        a.reverse();
        r.reverse();
        Self {
            answers: Mutex::new(a),
            replies: Mutex::new(r),
            ..Self::default()
        }
    }
    fn shown(&self) -> Vec<serde_json::Value> {
        self.shown.lock().unwrap().clone()
    }
    fn sent(&self) -> Vec<serde_json::Value> {
        self.sent.lock().unwrap().clone()
    }
}

impl ReviewTransport for Scripted {
    fn send(&self, _m: &str, p: &serde_json::Value) -> anyhow::Result<String> {
        self.sent.lock().unwrap().push(p.clone());
        self.replies
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(String::new()))
    }
    fn confirm_send(
        &self,
        _m: &str,
        p: &serde_json::Value,
        _o: &BTreeSet<String>,
    ) -> anyhow::Result<SendAnswer> {
        self.shown.lock().unwrap().push(p.clone());
        Ok(self
            .answers
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(SendAnswer::Send))
    }
    fn show(&self, text: &str) -> anyhow::Result<()> {
        self.printed.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

static OPEN: BTreeSet<String> = BTreeSet::new();

fn gate(boundary: bool, pre_confirmed: bool) -> SendGate<'static> {
    SendGate {
        boundary,
        pre_confirmed,
        open: &OPEN,
    }
}

fn note(text: &str) -> HumanNote {
    HumanNote {
        text: text.into(),
        target: None,
    }
}

/// The generator under test: the message is the pending note texts.
fn build(pending: &[HumanNote]) -> serde_json::Value {
    serde_json::json!({ "notes": pending.iter().map(|n| n.text.clone()).collect::<Vec<_>>() })
}

/// Run one gated turn; returns the outcome and every batch `on_consented` saw.
fn run(
    t: &Scripted,
    home: &std::path::Path,
    channel_id: &str,
    pending: &mut Vec<HumanNote>,
    g: SendGate,
    flush: impl Fn(&[HumanNote]) -> anyhow::Result<()>,
) -> (RetryOutcome, Vec<Vec<HumanNote>>) {
    let mut seen = Vec::new();
    let out = run_turn_with_retry(
        t,
        home,
        "review-x",
        "main",
        &build,
        pending,
        &mut |notes| {
            seen.push(notes.to_vec());
            flush(notes)
        },
        g,
        channel_id,
        Duration::ZERO,
    )
    .unwrap();
    (out, seen)
}

/// `Note` then `Send`: the note is rebuilt into the reprint, flushed once,
/// and the bytes sent are the bytes last shown (N5).
#[test]
fn note_then_send_flushes_once_and_sends_the_last_shown_message() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(vec![SendAnswer::Note(note("A")), SendAnswer::Send], vec![]);
    let mut pending = Vec::new();
    let (out, seen) = run(&t, tmp.path(), &ch, &mut pending, gate(true, false), |_| {
        Ok(())
    });

    assert!(matches!(out, RetryOutcome::Sent { .. }), "{out:?}");
    assert_eq!(seen, vec![vec![note("A")]]);
    assert!(pending.is_empty(), "flush empties the queue (N9)");
    let shown = t.shown();
    assert_eq!(shown, vec![build(&[]), build(&[note("A")])]);
    assert_eq!(t.sent(), vec![shown[1].clone()]);
}

/// AC-P3a-7 (driver half): `Note` then `Stop` → never flushed, nothing sent;
/// the queue is left for the caller to discard.
#[test]
fn note_then_stop_never_flushes() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(vec![SendAnswer::Note(note("A")), SendAnswer::Stop], vec![]);
    let mut pending = Vec::new();
    let (out, seen) = run(&t, tmp.path(), &ch, &mut pending, gate(true, false), |_| {
        Ok(())
    });

    assert_eq!(out, RetryOutcome::Stopped);
    assert!(seen.is_empty());
    assert!(t.sent().is_empty());
    assert_eq!(pending, vec![note("A")]);
}

/// P3a-§5.2: a boundary `/rule` does not flush and keeps the queue.
#[test]
fn boundary_rule_keeps_the_queue_unflushed() {
    let (tmp, ch) = setup_channel();
    let rule = RulingInput {
        finding: "F1".into(),
        decision: RulingDecision::Drop,
        text: "x".into(),
    };
    let t = Scripted::new(
        vec![
            SendAnswer::Note(note("A")),
            SendAnswer::SendWithRuling(rule.clone()),
        ],
        vec![],
    );
    let mut pending = Vec::new();
    let (out, seen) = run(&t, tmp.path(), &ch, &mut pending, gate(true, false), |_| {
        Ok(())
    });

    assert_eq!(out, RetryOutcome::RuleFirst(rule));
    assert!(seen.is_empty());
    assert_eq!(pending, vec![note("A")]);
}

/// AC-P3a-12: first send fails, retry succeeds → flushed exactly once, and
/// the retry re-sends the same bytes.
#[test]
fn ac_p3a_12_transport_retry_does_not_flush_again() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(
        vec![SendAnswer::Note(note("A")), SendAnswer::Send],
        vec![Err(anyhow::anyhow!("peer offline")), Ok("ok".into())],
    );
    let mut pending = Vec::new();
    let (out, seen) = run(
        &t,
        tmp.path(),
        &ch,
        &mut pending,
        gate(false, false),
        |_| Ok(()),
    );

    assert!(matches!(out, RetryOutcome::Sent { .. }), "{out:?}");
    assert_eq!(seen.len(), 1);
    let sent = t.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1]);
    assert_eq!(sent[0], build(&[note("A")]));
}

/// AC-P3a-13: the second of two appends fails → `paused { kind: other }`
/// whose reason says 1 of 2, printed, and nothing sent.
#[test]
fn ac_p3a_13_partial_flush_pauses_without_sending() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(vec![SendAnswer::Send], vec![]);
    let mut pending = vec![note("A"), note("B")];
    let (out, _) = run(&t, tmp.path(), &ch, &mut pending, gate(true, false), |_| {
        Err(FlushFailed {
            recorded: 1,
            total: 2,
            cause: "disk full".into(),
        }
        .into())
    });

    let RetryOutcome::Paused { reason } = out else {
        panic!("expected Paused, got {out:?}");
    };
    assert!(
        reason.starts_with("note flush failed after 1 of 2 notes;"),
        "{reason}"
    );
    assert!(reason.ends_with(": disk full"), "{reason}");
    assert!(t.sent().is_empty(), "no send after a failed flush");
    assert_eq!(*t.printed.lock().unwrap(), vec![reason.clone()]);

    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    let paused: Vec<_> = svc
        .load_events(&ch)
        .unwrap()
        .into_iter()
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => match env.payload {
                ReviewPayload::Paused { kind, reason, .. } => Some((kind, reason)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(paused, vec![(PauseKind::Other, reason)]);
}

/// A callback error that is not a [`FlushFailed`] is a real error, not a
/// pause; nothing is sent.
#[test]
fn other_flush_errors_propagate() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(vec![], vec![]);
    let err = run_turn_with_retry(
        &t,
        tmp.path(),
        "review-x",
        "main",
        &build,
        &mut vec![note("A")],
        &mut |_| Err(anyhow::anyhow!("boom")),
        gate(true, false),
        &ch,
        Duration::ZERO,
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "boom");
    assert!(t.sent().is_empty());
}

/// D1: `pre_confirmed` (the send after a boundary `/rule`) asks nothing but
/// still flushes, and sends the message built from the kept queue.
#[test]
fn pre_confirmed_flushes_without_a_prompt() {
    let (tmp, ch) = setup_channel();
    let t = Scripted::new(vec![], vec![]);
    let reviewer_note = HumanNote {
        text: "A".into(),
        target: Some(Role::Main),
    };
    let mut pending = vec![reviewer_note.clone()];
    let (out, seen) = run(&t, tmp.path(), &ch, &mut pending, gate(true, true), |_| {
        Ok(())
    });

    assert!(matches!(out, RetryOutcome::Sent { .. }), "{out:?}");
    assert!(t.shown().is_empty(), "no prompt");
    assert_eq!(seen, vec![vec![reviewer_note.clone()]]);
    assert!(pending.is_empty());
    assert_eq!(t.sent(), vec![build(&[reviewer_note])]);
}
