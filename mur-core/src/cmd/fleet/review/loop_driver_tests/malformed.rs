//! AC5 (§3.2 / §3.4): a malformed verdict is retried once with a validation
//! hint; a second malformed verdict yields `blocked`. Same for the main
//! agent's rebuttal.

use super::*;
use crate::cmd::fleet::review::wire::message_text;

/// Scripted per-member replies (in order) that also records every prompt.
struct Scripted {
    main: Mutex<Vec<String>>,
    reviewer: Mutex<Vec<String>>,
    seen: Mutex<Vec<(String, String)>>,
}

impl Scripted {
    fn new(main: &[&str], reviewer: &[&str]) -> Self {
        let rev = |v: &[&str]| v.iter().rev().map(|s| s.to_string()).collect();
        Self {
            main: Mutex::new(rev(main)),
            reviewer: Mutex::new(rev(reviewer)),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn prompts_to(&self, member: &str) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == member)
            .map(|(_, t)| t.clone())
            .collect()
    }
}

impl ReviewTransport for Scripted {
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        let text = message_text(params).unwrap_or_default().to_string();
        self.seen.lock().unwrap().push((member.to_string(), text));
        let queue = match member {
            "main" => &self.main,
            "reviewer" => &self.reviewer,
            other => panic!("unexpected member {other:?}"),
        };
        Ok(queue
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| panic!("{member} sent more turns than scripted")))
    }
}

fn run(
    t: &Scripted,
) -> (
    tempfile::TempDir,
    String,
    super::super::ledger::Ledger,
    LoopDriverStop,
) {
    let (tmp, channel_id) = setup_channel();
    let (ledger, stop) = run_review_loop(
        t,
        tmp.path(),
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &Instant::now,
    )
    .unwrap();
    (tmp, channel_id, ledger, stop)
}

const REVISE_ONE: &str =
    r#"{"verdict":"revise","findings":[{"severity":"low","issue":"needs a doc comment"}]}"#;
const APPROVE_F1: &str = r#"{"verdict":"approve","prior":[{"id":"F1","status":"resolved"}]}"#;
const ACCEPT_F1: &str =
    "fixed\n```json\n{\"responses\":[{\"id\":\"F1\",\"answer\":\"accept\"}]}\n```";

/// One malformed verdict, then a valid one: the retry carries the hint,
/// and the session continues to `approve`.
#[test]
fn malformed_verdict_is_retried_once_with_a_hint() {
    let t = Scripted::new(
        &["draft"],
        &["looks fine to me", r#"{"verdict":"approve"}"#],
    );
    let (tmp, channel_id, ledger, stop) = run(&t);
    assert_eq!(stop, LoopDriverStop::Approve);
    let prompts = t.prompts_to("reviewer");
    assert_eq!(prompts.len(), 2, "exactly one retry");
    assert!(!prompts[0].contains("could not be accepted"));
    assert!(prompts[1].contains("could not be accepted: no JSON verdict block found"));
    assert_eq!(
        fold_rounds(&read_payloads(tmp.path(), &channel_id)).unwrap(),
        ledger
    );
}

/// Malformed twice → `blocked`, with a `blocked` verdict signed for the round.
#[test]
fn second_malformed_verdict_is_blocked() {
    let t = Scripted::new(&["draft"], &["nope", "still nope"]);
    let (tmp, channel_id, ledger, stop) = run(&t);
    assert_eq!(
        stop,
        LoopDriverStop::Blocked {
            role: Role::Reviewer
        }
    );
    assert_eq!(t.prompts_to("reviewer").len(), 2);
    let payloads = read_payloads(tmp.path(), &channel_id);
    assert!(payloads.iter().any(|p| matches!(
        p,
        ReviewPayload::Verdict {
            kind: super::super::schema::VerdictKind::Blocked,
            ..
        }
    )));
    assert_eq!(fold_rounds(&payloads).unwrap(), ledger);
}

/// §3.3: a missing status for an open finding is malformed too.
#[test]
fn missing_prior_status_is_malformed_and_hinted() {
    let t = Scripted::new(
        &["draft", ACCEPT_F1],
        &[REVISE_ONE, r#"{"verdict":"approve"}"#, APPROVE_F1],
    );
    let (_tmp, _c, _l, stop) = run(&t);
    assert_eq!(stop, LoopDriverStop::Approve);
    let prompts = t.prompts_to("reviewer");
    assert_eq!(prompts.len(), 3);
    assert!(
        prompts[2].contains("no status for open finding(s): F1"),
        "{}",
        prompts[2]
    );
}

/// §3.4: main's rebuttal is validated the same way — retried once, and a
/// valid retry is signed as a `rebuttal` event.
#[test]
fn malformed_rebuttal_is_retried_once_then_recorded() {
    let t = Scripted::new(&["draft", "fixed it", ACCEPT_F1], &[REVISE_ONE, APPROVE_F1]);
    let (tmp, channel_id, ledger, stop) = run(&t);
    assert_eq!(stop, LoopDriverStop::Approve);
    let prompts = t.prompts_to("main");
    assert_eq!(prompts.len(), 3);
    assert!(prompts[2].contains("could not be accepted: no JSON responses block found"));
    let payloads = read_payloads(tmp.path(), &channel_id);
    assert!(
        payloads
            .iter()
            .any(|p| matches!(p, ReviewPayload::Rebuttal { round: 2, .. }))
    );
    assert_eq!(fold_rounds(&payloads).unwrap(), ledger);
}

/// §3.4: a second malformed rebuttal → `blocked`, and the reviewer is never
/// sent a round built on it.
#[test]
fn second_malformed_rebuttal_is_blocked() {
    let reject_no_reason = r#"{"responses":[{"id":"F1","answer":"reject"}]}"#;
    let t = Scripted::new(
        &["draft", reject_no_reason, reject_no_reason],
        &[REVISE_ONE],
    );
    let (_tmp, _c, _l, stop) = run(&t);
    assert_eq!(stop, LoopDriverStop::Blocked { role: Role::Main });
    assert_eq!(t.prompts_to("reviewer").len(), 1);
    assert!(t.prompts_to("main")[2].contains("a reason is required"));
}

/// AC10 through the driver: an `approve` that disputes a HIGH finding is
/// re-sent with the refusal as the hint; a second one ends `blocked`, and
/// no `approve` verdict is ever signed.
#[test]
fn ac10_approve_over_disputed_high_is_hinted_then_blocked() {
    let revise_high =
        r#"{"verdict":"revise","findings":[{"severity":"high","issue":"panics on input"}]}"#;
    let approve_disputed = r#"{"verdict":"approve","prior":[{"id":"F1","status":"disputed","reason":"still panics"}]}"#;
    let reject_f1 = "no\n```json\n{\"responses\":[{\"id\":\"F1\",\"answer\":\"reject\",\"reason\":\"by design\"}]}\n```";
    let t = Scripted::new(
        &["draft", reject_f1],
        &[revise_high, approve_disputed, approve_disputed],
    );
    let (tmp, channel_id, ledger, stop) = run(&t);
    assert_eq!(
        stop,
        LoopDriverStop::Blocked {
            role: Role::Reviewer
        }
    );
    let prompts = t.prompts_to("reviewer");
    assert_eq!(prompts.len(), 3);
    assert!(
        prompts[2].contains("`approve` is refused while a high-severity finding is disputed (F1)"),
        "{}",
        prompts[2]
    );
    let payloads = read_payloads(tmp.path(), &channel_id);
    assert!(!payloads.iter().any(|p| matches!(
        p,
        ReviewPayload::Verdict {
            kind: super::super::schema::VerdictKind::Approve,
            ..
        }
    )));
    assert_eq!(fold_rounds(&payloads).unwrap(), ledger);
}
