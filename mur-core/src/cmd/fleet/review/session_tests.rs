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
        fn confirm_send(&self, _m: &str, _p: &serde_json::Value) -> Result<bool> {
            Ok(false)
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
