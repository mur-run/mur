//! Tests for `session.rs`: the `mur fleet review` lifecycle around the loop.

use std::time::Duration;

use mur_common::channel::EventKind;
use mur_common::limits::Stuck;

use super::*;
use crate::cmd::fleet::review::schema::{NoteClassification, classify_note_payload};

/// Main says anything; the reviewer replies from a fixed script.
struct Scripted {
    reviewer: std::sync::Mutex<Vec<String>>,
}

impl ReviewTransport for Scripted {
    fn send(&self, member: &str, _params: &serde_json::Value) -> Result<String> {
        if member == "reviewer" {
            Ok(self.reviewer.lock().unwrap().remove(0))
        } else {
            Ok("done".into())
        }
    }
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

fn limits() -> SessionLimits {
    SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None)
}

/// §7.1 / A1 / §4: the session ends with `session_stopped` (reason and
/// unresolved IDs), the fleet definition is gone, the channel is kept.
#[test]
fn session_end_removes_fleet_keeps_channel_and_records_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let fleet = create_session_fleet(home, "review-test0001", "main", "reviewer", "task").unwrap();
    assert!(store::fleet_dir(home, &fleet.name).exists());

    let transport = Scripted {
        reviewer: std::sync::Mutex::new(vec![
            r#"{"verdict":"blocked","findings":[{"severity":"medium","issue":"no tests"}]}"#.into(),
        ]),
    };
    let (ledger, stop) = run_session(
        &transport,
        home,
        &fleet,
        "add a test",
        limits(),
        Duration::ZERO,
    )
    .unwrap();
    assert_eq!(stop, LoopDriverStop::ReviewerBlocked);

    assert!(
        !store::fleet_dir(home, &fleet.name).exists(),
        "definition removed"
    );
    let events = payloads(home, &fleet.channel_id);
    match events.last() {
        Some(ReviewPayload::SessionStopped {
            reason, unresolved, ..
        }) => {
            assert_eq!(reason, "blocked");
            assert_eq!(unresolved, &vec!["F1".to_string()]);
        }
        other => panic!("last event must be session_stopped, got {other:?}"),
    }
    // The kept channel still folds to the same ledger the loop returned.
    let replayed = crate::cmd::fleet::review::ledger::fold_rounds(&events).unwrap();
    assert_eq!(replayed.open_set(), ledger.open_set());
}

/// A declined send ends the session as `stopped` and still cleans up.
#[test]
fn declining_the_first_send_stops_and_cleans_up() {
    struct Declines;
    impl ReviewTransport for Declines {
        fn send(&self, _m: &str, _p: &serde_json::Value) -> Result<String> {
            panic!("must not send after a declined gate");
        }
        fn confirm_send(
            &self,
            _m: &str,
            _p: &serde_json::Value,
            _o: &std::collections::BTreeSet<String>,
        ) -> Result<crate::cmd::fleet::review::driver::SendAnswer> {
            Ok(crate::cmd::fleet::review::driver::SendAnswer::Stop)
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let fleet = create_session_fleet(home, "review-test0002", "main", "reviewer", "task").unwrap();
    let (_, stop) = run_session(&Declines, home, &fleet, "t", limits(), Duration::ZERO).unwrap();
    assert_eq!(stop, LoopDriverStop::Stopped);
    assert!(!store::fleet_dir(home, &fleet.name).exists());
}

#[test]
fn session_names_carry_the_reserved_prefix_and_are_valid() {
    let name = new_session_name();
    assert!(name.starts_with(REVIEW_FLEET_PREFIX));
    assert!(mur_common::fleet::valid_fleet_name(&name));
    assert_ne!(name, new_session_name());
}

#[test]
fn only_enter_or_yes_sends_and_eof_never_does() {
    assert!(is_send_answer("\n"));
    assert!(is_send_answer("y\n"));
    assert!(is_send_answer("YES\n"));
    assert!(!is_send_answer("q\n"));
    assert!(!is_send_answer("no\n"));
    assert!(!is_send_answer(""), "EOF must decline");
}

/// §8.3: after approve, disputed findings are listed before open ones.
#[test]
fn stop_screen_lists_reason_and_unresolved_findings() {
    use crate::cmd::fleet::review::schema::{FindingStatus, Severity};
    let mut ledger = Ledger::default();
    for (i, issue) in ["open one", "disputed one"].iter().enumerate() {
        ledger
            .apply(&ReviewPayload::FindingIssued {
                round: 1,
                id: format!("F{}", i + 1),
                severity: Severity::Low,
                issue: issue.to_string(),
            })
            .unwrap();
    }
    ledger
        .apply(&ReviewPayload::FindingStatus {
            round: 2,
            id: "F2".into(),
            status: FindingStatus::Disputed,
            reason: None,
        })
        .unwrap();
    let screen = render_stop_screen(&LoopDriverStop::Approve, &ledger, "ch-1");
    assert!(screen.starts_with("Review stopped: approve"));
    let (f2, f1) = (screen.find("F2").unwrap(), screen.find("F1").unwrap());
    assert!(f2 < f1, "disputed first after approve:\n{screen}");
    assert!(screen.contains("Channel kept for audit: ch-1"));
}

/// Ledger with F1 (high, open) and F2 (low, open), both issued in round 1.
fn ledger_with_open_high() -> Ledger {
    use crate::cmd::fleet::review::schema::Severity;
    let mut ledger = Ledger::default();
    for (id, severity) in [("F1", Severity::High), ("F2", Severity::Low)] {
        ledger
            .apply(&ReviewPayload::FindingIssued {
                round: 1,
                id: id.into(),
                severity,
                issue: format!("{id} issue"),
            })
            .unwrap();
    }
    ledger
}

/// #1721 default (option B): an approve over an open high finding leads
/// the stop screen with a warning naming those IDs.
#[test]
fn approve_with_open_high_finding_leads_with_a_warning() {
    let screen = render_stop_screen(&LoopDriverStop::Approve, &ledger_with_open_high(), "ch-1");
    let warn = screen
        .find(OPEN_HIGH_APPROVE_WARNING)
        .unwrap_or_else(|| panic!("missing warning:\n{screen}"));
    let warn_line = screen[warn..].lines().next().unwrap();
    assert!(warn_line.contains("F1"), "{warn_line}");
    assert!(!warn_line.contains("F2"), "low finding named: {warn_line}");
    assert!(
        warn < screen.find("Unresolved findings:").unwrap(),
        "warning must precede the list:\n{screen}"
    );
}

#[test]
fn open_high_warning_only_on_approve() {
    let ledger = ledger_with_open_high();
    let screen = render_stop_screen(&LoopDriverStop::Guard(LoopStop::Deadline), &ledger, "ch-1");
    assert!(!screen.contains(OPEN_HIGH_APPROVE_WARNING), "{screen}");
}

#[test]
fn approve_without_open_high_has_no_warning() {
    use crate::cmd::fleet::review::schema::Severity;
    let mut ledger = Ledger::default();
    ledger
        .apply(&ReviewPayload::FindingIssued {
            round: 1,
            id: "F1".into(),
            severity: Severity::Medium,
            issue: "medium".into(),
        })
        .unwrap();
    let screen = render_stop_screen(&LoopDriverStop::Approve, &ledger, "ch-1");
    assert!(!screen.contains(OPEN_HIGH_APPROVE_WARNING), "{screen}");
}

#[test]
fn stop_reasons_name_the_limit() {
    assert_eq!(
        stop_reason(&LoopDriverStop::Guard(LoopStop::Deadline)),
        "limit: deadline"
    );
    assert_eq!(
        stop_reason(&LoopDriverStop::Guard(LoopStop::Budget)),
        "limit: cost_usd"
    );
}

#[test]
fn transport_pause_reason_is_not_prefixed_twice() {
    // driver.rs already writes "transport failure after one retry: …".
    let reason = "transport failure after one retry: agent 'qa' is not running".to_string();
    let got = stop_reason(&LoopDriverStop::Paused {
        reason: reason.clone(),
    });
    assert_eq!(got, reason);
    assert_eq!(got.matches("transport failure").count(), 1);
}

#[test]
fn preflight_names_every_member_that_is_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let up = home.join("agents").join("main");
    std::fs::create_dir_all(&up).unwrap();
    std::fs::write(up.join(RUNNING_LOCK), "{}").unwrap();

    assert!(require_running(home, &["main"]).is_ok());

    let err = require_running(home, &["main", "qa", "other"])
        .unwrap_err()
        .to_string();
    assert!(err.contains("qa") && err.contains("other"), "{err}");
    assert!(!err.contains("'main'"), "{err}");
    assert!(err.contains("mur agent start qa"), "{err}");
}

/// The stop screen names a failed member task as such, not as a
/// malformed verdict (the run-3 symptom: `blocked (malformed verdict)`
/// over an empty reviewer reply after `hitl_denied`).
#[test]
fn task_failed_stop_reason_names_member_and_cause() {
    let got = stop_reason(&LoopDriverStop::TaskFailed {
        member: "qa".into(),
        cause: "tool call denied: timed out".into(),
    });
    assert_eq!(got, "qa task failed: tool call denied: timed out");
    assert!(!got.contains("malformed"));
}

/// AC4a wiring: the terminal gate reports the time spent at its prompts
/// (send prompt and tool-approval prompt share one counter) exactly once.
#[test]
fn terminal_gate_reports_prompt_time_once() {
    use super::{HumanWait, TerminalGate};
    use crate::cmd::fleet::review::driver::ReviewTransport;
    struct Nop;
    impl ReviewTransport for Nop {
        fn send(&self, _: &str, _: &serde_json::Value) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }
    let wait = HumanWait::default();
    wait.time(|| std::thread::sleep(std::time::Duration::from_millis(20)));
    let gate = TerminalGate {
        inner: Nop,
        wait: &wait,
        input: &|| Ok(String::new()),
        output: &|_| Ok(()),
    };
    assert!(gate.take_human_wait() >= std::time::Duration::from_millis(20));
    assert_eq!(gate.take_human_wait(), std::time::Duration::ZERO);
}

// ---- P2 Task 6: the send prompt and the ruling prompt ----

mod prompts {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    use super::super::{HumanWait, TerminalGate};
    use crate::cmd::fleet::review::constants::RULING_PROMPT;
    use crate::cmd::fleet::review::driver::{ReviewTransport, SendAnswer};
    use crate::cmd::fleet::review::ledger::Ledger;
    use crate::cmd::fleet::review::ruling::RulingInput;
    use crate::cmd::fleet::review::schema::{
        Cumulative, FindingStatus, RebuttalAnswer, RebuttalResponseDto, ReviewPayload,
        RulingDecision, Severity,
    };

    struct Nop;
    impl ReviewTransport for Nop {
        fn send(&self, _: &str, _: &serde_json::Value) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    /// A terminal that answers from `lines` (front first) and records output.
    struct Term {
        lines: RefCell<Vec<String>>,
        reads: RefCell<usize>,
        out: RefCell<String>,
    }

    impl Term {
        fn new(lines: &[&str]) -> Self {
            Self {
                lines: RefCell::new(lines.iter().rev().map(|l| l.to_string()).collect()),
                reads: RefCell::new(0),
                out: RefCell::new(String::new()),
            }
        }
        fn read(&self) -> std::io::Result<String> {
            *self.reads.borrow_mut() += 1;
            Ok(self.lines.borrow_mut().pop().unwrap_or_default())
        }
        fn write(&self, s: &str) -> std::io::Result<()> {
            self.out.borrow_mut().push_str(s);
            Ok(())
        }
    }

    fn confirm(lines: &[&str], open: &[&str]) -> (SendAnswer, Term) {
        let term = Term::new(lines);
        let wait = HumanWait::default();
        let input = || term.read();
        let output = |s: &str| term.write(s);
        let gate = TerminalGate {
            inner: Nop,
            wait: &wait,
            input: &input,
            output: &output,
        };
        let open: BTreeSet<String> = open.iter().map(|s| s.to_string()).collect();
        let answer = gate
            .confirm_send("main", &serde_json::json!({}), &open)
            .unwrap();
        (answer, term)
    }

    #[test]
    fn send_prompt_rule_is_send_with_ruling() {
        let (answer, term) = confirm(&["/rule drop F1 x\n"], &["F1"]);
        assert_eq!(
            answer,
            SendAnswer::SendWithRuling(RulingInput {
                finding: "F1".into(),
                decision: RulingDecision::Drop,
                text: "x".into(),
            })
        );
        assert_eq!(*term.reads.borrow(), 1, "no second prompt");
    }

    #[test]
    fn send_prompt_rejects_rule_on_closed_finding() {
        let (answer, term) = confirm(&["/rule drop F1 x\n", "\n"], &[]);
        assert_eq!(answer, SendAnswer::Send);
        assert_eq!(*term.reads.borrow(), 2, "re-prompted");
        assert!(term.out.borrow().contains("F1 is not an open finding"));
    }

    #[test]
    fn send_prompt_p1_answers_unchanged() {
        for line in ["\n", "y\n", "yes\n"] {
            assert_eq!(confirm(&[line], &[]).0, SendAnswer::Send, "{line:?}");
        }
        for line in ["q\n", "nope\n", ""] {
            assert_eq!(confirm(&[line], &[]).0, SendAnswer::Stop, "{line:?}");
        }
    }

    /// F1 issued, disputed, rejected twice by main ("still wrong" last).
    fn escalated() -> Ledger {
        let mut l = Ledger::default();
        let id = l.next_finding_id();
        let zero = || Cumulative {
            exec_time_ms: 0,
            cost_usd_micros: 0,
        };
        l.apply(&ReviewPayload::FindingIssued {
            round: 1,
            id: id.clone(),
            severity: Severity::High,
            issue: "null deref in parse".into(),
        })
        .unwrap();
        l.apply(&ReviewPayload::FindingStatus {
            round: 1,
            id: id.clone(),
            status: FindingStatus::Disputed,
            reason: None,
        })
        .unwrap();
        for (round, reason) in [(1, "not reachable"), (2, "still wrong")] {
            l.apply(&ReviewPayload::Rebuttal {
                round,
                responses: vec![RebuttalResponseDto {
                    id: id.clone(),
                    answer: RebuttalAnswer::Reject,
                    reason: Some(reason.into()),
                }],
                cumulative: zero(),
            })
            .unwrap();
        }
        assert_eq!(l.pending_ruling().len(), 1);
        l
    }

    #[test]
    fn ask_ruling_prints_both_positions_and_prompt() {
        let ledger = escalated();
        let term = Term::new(&["q\n"]);
        let wait = HumanWait::default();
        let input = || term.read();
        let output = |s: &str| term.write(s);
        let gate = TerminalGate {
            inner: Nop,
            wait: &wait,
            input: &input,
            output: &output,
        };
        let line = gate
            .ask_ruling(ledger.pending_ruling()[0], &ledger)
            .unwrap();
        assert_eq!(line, "q\n", "raw line, unparsed");
        let out = term.out.borrow();
        assert!(out.contains("null deref in parse"), "{out}");
        assert!(out.contains("still wrong"), "{out}");
        assert!(out.contains(&RULING_PROMPT.replace("{id}", "F1")), "{out}");
    }

    #[test]
    fn ask_ruling_time_counts_as_human_wait() {
        let ledger = escalated();
        let wait = HumanWait::default();
        let input = || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            Ok(String::new())
        };
        let gate = TerminalGate {
            inner: Nop,
            wait: &wait,
            input: &input,
            output: &|_| Ok(()),
        };
        gate.ask_ruling(ledger.pending_ruling()[0], &ledger)
            .unwrap();
        assert!(gate.take_human_wait() >= std::time::Duration::from_millis(20));
    }
}

/// P2 Task 8: what `run_session` does around an escalation the loop now
/// waits on (AC-P2-1, AC-P2-13 session halves).
mod escalation {
    use super::*;
    use crate::cmd::fleet::review::ledger::EscalationRecord;
    use crate::cmd::fleet::review::resume::prepare_resume;
    use crate::cmd::fleet::review::wire::message_text;

    const ISSUE_F1: &str =
        r#"{"verdict":"revise","findings":[{"severity":"high","issue":"unchecked unwrap"}]}"#;
    const DISPUTE_F1: &str =
        r#"{"verdict":"revise","prior":[{"id":"F1","status":"disputed","reason":"still panics"}]}"#;

    /// Main rejects every listed finding, so F1 escalates at the round-3
    /// seal; the ruling prompt answers `ask`.
    struct Escalating {
        reviewer: std::sync::Mutex<Vec<String>>,
        ask: &'static str,
    }

    impl Escalating {
        fn new(ask: &'static str) -> Self {
            let script = [DISPUTE_F1, DISPUTE_F1, ISSUE_F1]
                .map(String::from)
                .to_vec();
            Self {
                reviewer: std::sync::Mutex::new(script),
                ask,
            }
        }
    }

    impl ReviewTransport for Escalating {
        fn send(&self, member: &str, params: &serde_json::Value) -> Result<String> {
            if member == "reviewer" {
                return Ok(self.reviewer.lock().unwrap().pop().expect("script"));
            }
            let prompt = message_text(params).unwrap_or_default();
            let responses: Vec<serde_json::Value> = prompt
                .lines()
                .filter_map(|l| l.strip_prefix("- "))
                .filter_map(|l| l.split_once(" ["))
                .map(|(id, _)| id)
                .filter(|id| id.starts_with('F'))
                .map(|id| serde_json::json!({"id": id, "answer": "reject", "reason": "no"}))
                .collect();
            Ok(format!(
                "done\n```json\n{}\n```",
                serde_json::json!({ "responses": responses })
            ))
        }

        fn ask_ruling(&self, _p: &EscalationRecord, _l: &Ledger) -> Result<String> {
            Ok(self.ask.to_string())
        }
    }

    fn run(ask: &'static str, name: &str) -> (tempfile::TempDir, Fleet, LoopDriverStop) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        crate::channel_writer::plant_writer_identity(home);
        let fleet = create_session_fleet(home, name, "main", "reviewer", "task").unwrap();
        let (_, stop) = run_session(
            &Escalating::new(ask),
            home,
            &fleet,
            "t",
            limits(),
            Duration::ZERO,
        )
        .unwrap();
        (tmp, fleet, stop)
    }

    /// AC-P2-1: EOF at the ruling prompt leaves the session paused — the
    /// definition stays and nothing records a stop.
    #[test]
    fn eof_at_ruling_prompt_keeps_the_session() {
        let (tmp, fleet, stop) = run("", "review-test0101");
        assert!(matches!(stop, LoopDriverStop::Paused { .. }), "{stop:?}");
        assert!(store::fleet_dir(tmp.path(), &fleet.name).exists());
        let events = payloads(tmp.path(), &fleet.channel_id);
        assert!(
            !events
                .iter()
                .any(|p| matches!(p, ReviewPayload::SessionStopped { .. })),
            "no session_stopped: {events:?}"
        );
    }

    /// AC-P2-13: `/abandon` ends the session as `escalation`, removes the
    /// definition, and `review-resume` refuses (on the missing definition,
    /// before it would reach the `session_stopped` check).
    #[test]
    fn abandon_stops_and_cannot_resume() {
        let (tmp, fleet, stop) = run("/abandon\n", "review-test0102");
        assert_eq!(stop, LoopDriverStop::Escalation);
        assert!(!store::fleet_dir(tmp.path(), &fleet.name).exists());
        match payloads(tmp.path(), &fleet.channel_id).last() {
            Some(ReviewPayload::SessionStopped { reason, .. }) => {
                assert_eq!(reason, "escalation")
            }
            other => panic!("last event must be session_stopped, got {other:?}"),
        }
        let err = prepare_resume(tmp.path(), &fleet.name)
            .unwrap_err()
            .to_string();
        assert!(err.contains("has ended"), "{err}");
    }

    /// A pause for a ruling shows the same resume hint as any pause.
    #[test]
    fn stop_screen_for_escalation_pause() {
        let stop = LoopDriverStop::Paused {
            reason: crate::cmd::fleet::review::constants::REVIEW_PAUSE_REASON_ESCALATION.into(),
        };
        let screen = render_stop_screen(&stop, &Ledger::default(), "fleet-review-test0103");
        assert!(
            screen.contains("mur fleet review-resume review-test0103"),
            "{screen}"
        );
    }
}
