//! AC2: paused → resumed continues at the same round with the same ledger.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use mur_common::channel::EventKind;
use mur_common::limits::Stuck;

use super::{prepare_resume, resume_session};
use crate::cmd::fleet::review::driver::ReviewTransport;
use crate::cmd::fleet::review::loop_driver::LoopDriverStop;
use crate::cmd::fleet::review::schema::{
    NoteClassification, ReviewPayload, SessionLimits, classify_note_payload,
};
use crate::cmd::fleet::review::session::{create_session_fleet, run_session};
use crate::cmd::fleet::review::wire::message_text;
use crate::cmd::fleet::store;

const REVISE_ONE: &str =
    r#"{"verdict":"revise","findings":[{"severity":"high","issue":"unchecked unwrap"}]}"#;
const APPROVE_F1: &str = r#"{"verdict":"approve","prior":[{"id":"F1","status":"resolved"}]}"#;
const ACCEPT_F1: &str =
    "fixed\n```json\n{\"responses\":[{\"id\":\"F1\",\"answer\":\"accept\"}]}\n```";

/// Scripted replies per member; `Err` entries model an offline member.
/// Records every (member, prompt) it is asked to send.
struct Scripted {
    main: Mutex<Vec<Result<String, String>>>,
    reviewer: Mutex<Vec<Result<String, String>>>,
    seen: Mutex<Vec<(String, String)>>,
}

impl Scripted {
    fn new(main: Vec<Result<&str, &str>>, reviewer: Vec<Result<&str, &str>>) -> Self {
        let own = |v: Vec<Result<&str, &str>>| {
            v.into_iter()
                .rev()
                .map(|r| r.map(str::to_string).map_err(str::to_string))
                .collect()
        };
        Self {
            main: Mutex::new(own(main)),
            reviewer: Mutex::new(own(reviewer)),
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl ReviewTransport for Scripted {
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        let text = message_text(params).unwrap_or_default().to_string();
        self.seen.lock().unwrap().push((member.to_string(), text));
        let q = if member == "main" {
            &self.main
        } else {
            &self.reviewer
        };
        match q.lock().unwrap().pop() {
            Some(Ok(s)) => Ok(s),
            Some(Err(e)) => anyhow::bail!(e),
            None => panic!("{member} sent more turns than scripted"),
        }
    }
}

fn limits() -> SessionLimits {
    SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None)
}

fn payloads(home: &Path, channel_id: &str) -> Vec<ReviewPayload> {
    mur_channel::ChannelService::open(home)
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

/// Round 1 completes (F1 issued), then the reviewer goes offline in round 2
/// and the session pauses (§8.1). Returns the home and fleet name.
fn paused_in_round_two() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let fleet =
        create_session_fleet(home, "review-resu0001", "main", "reviewer", "the task").unwrap();
    let t = Scripted::new(
        vec![Ok("draft"), Ok(ACCEPT_F1)],
        vec![Ok(REVISE_ONE), Err("offline"), Err("offline")],
    );
    let (_, stop) = run_session(&t, home, &fleet, "the task", limits(), Duration::ZERO).unwrap();
    assert!(matches!(stop, LoopDriverStop::Paused { .. }), "{stop:?}");
    (tmp, fleet.name)
}

#[test]
fn a_paused_session_keeps_its_fleet_and_writes_no_session_stopped() {
    let (tmp, name) = paused_in_round_two();
    assert!(store::fleet_path(tmp.path(), &name).exists());
    let fleet = store::load_fleet(tmp.path(), &name).unwrap();
    assert!(
        payloads(tmp.path(), &fleet.channel_id)
            .iter()
            .all(|p| !matches!(p, ReviewPayload::SessionStopped { .. }))
    );
}

/// AC2: the rebuilt ledger is the ledger at the pause, and the loop resumes
/// at round 2 from main's turn — not round 1, not round 3.
#[test]
fn resume_continues_at_the_same_round_with_the_same_ledger() {
    let (tmp, name) = paused_in_round_two();
    let home = tmp.path();
    let r = prepare_resume(home, &name).unwrap();
    assert_eq!(r.round, 2);
    assert_eq!(r.ledger.findings.len(), 1);
    assert_eq!(r.ledger.findings[0].id, "F1");
    assert!(r.ledger.paused);
    let before_findings = r.ledger.findings.clone();

    let t = Scripted::new(vec![Ok(ACCEPT_F1)], vec![Ok(APPROVE_F1)]);
    let (ledger, stop) = resume_session(&t, home, r, Duration::ZERO).unwrap();
    assert_eq!(stop, LoopDriverStop::Approve);
    assert_eq!(ledger.round, 2, "the resumed round is round 2");
    assert_eq!(
        ledger.findings.len(),
        before_findings.len(),
        "no finding lost or duplicated"
    );
    assert!(!ledger.paused);

    let seen = t.seen.lock().unwrap();
    assert_eq!(seen[0].0, "main", "resume starts from main's turn");
    assert!(seen[0].1.contains("round 2"), "{}", seen[0].1);
    assert!(seen[0].1.contains("- F1 [high, open]"), "{}", seen[0].1);
    drop(seen);

    // Session ended normally: stopped, fleet removed, channel replays cleanly.
    let fleet_channel = format!("fleet-{name}");
    let all = payloads(home, &fleet_channel);
    assert!(
        all.iter()
            .any(|p| matches!(p, ReviewPayload::Resumed { .. }))
    );
    assert!(
        all.iter()
            .any(|p| matches!(p, ReviewPayload::SessionStopped { .. }))
    );
    assert!(!store::fleet_path(home, &name).exists());
    // A reject/accept from the interrupted round was never signed, so each
    // round carries exactly one rebuttal.
    let rebuttals = all
        .iter()
        .filter(|p| matches!(p, ReviewPayload::Rebuttal { .. }))
        .count();
    assert_eq!(rebuttals, 1);
}

#[test]
fn resume_refuses_a_session_that_is_not_paused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let fleet = create_session_fleet(home, "review-live0001", "main", "reviewer", "t").unwrap();
    let err = prepare_resume(home, &fleet.name).unwrap_err().to_string();
    assert!(err.contains("session_started"), "{err}");
}

#[test]
fn resume_refuses_an_ended_session_and_a_non_review_name() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    assert!(
        prepare_resume(home, "dev")
            .unwrap_err()
            .to_string()
            .contains("not a review session")
    );
    assert!(
        prepare_resume(home, "review-gone0001")
            .unwrap_err()
            .to_string()
            .contains("has ended")
    );
}

/// §8.2: a damaged channel is never resumed silently.
#[test]
fn resume_refuses_a_damaged_channel_and_names_the_line() {
    let (tmp, name) = paused_in_round_two();
    let home = tmp.path();
    let path = mur_channel::ChannelService::open(home)
        .unwrap()
        .store()
        .events_path(&format!("fleet-{name}"));
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("{truncated\n");
    std::fs::write(&path, text).unwrap();
    let err = prepare_resume(home, &name).unwrap_err().to_string();
    assert!(err.contains("cannot be resumed"), "{err}");
    assert!(err.contains("line"), "{err}");
}

/// Append the payloads a SIGKILLed driver would have left: round 2 started
/// and main's rebuttal (a reject of F1) signed, but no verdict, no `paused`.
fn crashed_in_round_two() -> (tempfile::TempDir, String) {
    use crate::cmd::fleet::review::schema::{
        RebuttalAnswer, RebuttalResponseDto, Role, to_note_payload,
    };
    use crate::cmd::fleet::review::verdict::zero_cumulative;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let fleet =
        create_session_fleet(home, "review-crsh0001", "main", "reviewer", "the task").unwrap();
    let svc = mur_channel::ChannelService::open(home).unwrap();
    let started = ReviewPayload::SessionStarted {
        members: ["main".into(), "reviewer".into()],
        mode: crate::cmd::fleet::review::schema::Mode::Auto,
        limits: limits(),
    };
    let sent = |round, to| ReviewPayload::TurnSent {
        round,
        to,
        restart_note: None,
        human_wait_ms: 0,
    };
    let log = vec![
        started,
        sent(1, Role::Main),
        sent(1, Role::Reviewer),
        ReviewPayload::FindingIssued {
            round: 1,
            id: "F1".into(),
            severity: crate::cmd::fleet::review::schema::Severity::High,
            issue: "unchecked unwrap".into(),
        },
        ReviewPayload::Verdict {
            round: 1,
            kind: crate::cmd::fleet::review::schema::VerdictKind::Revise,
            cumulative: zero_cumulative(),
        },
        sent(2, Role::Main),
        sent(2, Role::Reviewer),
        ReviewPayload::Rebuttal {
            round: 2,
            responses: vec![RebuttalResponseDto {
                id: "F1".into(),
                answer: RebuttalAnswer::Reject,
                reason: Some("disagree".into()),
            }],
            cumulative: zero_cumulative(),
        },
    ];
    for p in &log {
        crate::channel_writer::append_as_writer(
            &svc,
            home,
            &fleet.channel_id,
            crate::channel_writer::ROUTER_AGENT,
            mur_common::channel::ChannelActor::System,
            EventKind::Note,
            to_note_payload(p),
            None,
        )
        .unwrap();
    }
    (tmp, fleet.name)
}

/// AC15c: a crashed session resumes at the round after the last SEALED
/// round, records `paused`(crashed) at prepare and `resumed` at resume, and the reject from the
/// unsealed attempt is not counted.
#[test]
fn a_crashed_session_resumes_after_the_last_sealed_round() {
    use crate::cmd::fleet::review::constants::REVIEW_PAUSE_REASON_CRASHED;
    let (tmp, name) = crashed_in_round_two();
    let home = tmp.path();
    let before = payloads(home, &format!("fleet-{name}")).len();
    let r = prepare_resume(home, &name).unwrap();
    assert!(r.crashed);
    assert_eq!(r.round, 2);
    // P2-§6 / D2: the crashed `paused` is written by prepare_resume, under
    // the lock and before any prompt — not later by resume_session.
    let at_prepare = payloads(home, &format!("fleet-{name}"));
    assert_eq!(at_prepare.len(), before + 1);
    assert!(matches!(
        at_prepare.last(),
        Some(ReviewPayload::Paused { reason, .. }) if reason == REVIEW_PAUSE_REASON_CRASHED
    ));
    assert_eq!(
        r.ledger.findings[0].reject_count, 0,
        "unsealed reject dropped"
    );

    let t = Scripted::new(vec![Ok(ACCEPT_F1)], vec![Ok(APPROVE_F1)]);
    let (ledger, stop) = resume_session(&t, home, r, Duration::ZERO).unwrap();
    assert_eq!(stop, LoopDriverStop::Approve);
    assert_eq!(ledger.findings[0].reject_count, 0);
    assert!(ledger.escalations.is_empty());
    assert_eq!(
        t.seen.lock().unwrap()[0].0,
        "main",
        "re-run from main's turn"
    );

    let all = payloads(home, &format!("fleet-{name}"));
    let i_paused = all
        .iter()
        .position(|p| matches!(p, ReviewPayload::Paused { reason, .. } if reason == REVIEW_PAUSE_REASON_CRASHED))
        .expect("paused(crashed) recorded");
    assert!(matches!(all[i_paused + 1], ReviewPayload::Resumed { .. }));
}

/// AC15c: semi-auto after a crash — the resumed run sends nothing without
/// the gate; the loop is driven in semi-auto (`Mode` from the original
/// `session_started` was Auto). Checked via the session banner: resume
/// never writes `mode_changed` to auto.
#[test]
fn a_crashed_resume_never_restores_auto() {
    let (tmp, name) = crashed_in_round_two();
    let home = tmp.path();
    let r = prepare_resume(home, &name).unwrap();
    let t = Scripted::new(vec![Ok(ACCEPT_F1)], vec![Ok(APPROVE_F1)]);
    resume_session(&t, home, r, Duration::ZERO).unwrap();
    let all = payloads(home, &format!("fleet-{name}"));
    assert!(!all.iter().any(|p| matches!(
        p,
        ReviewPayload::ModeChanged {
            mode: crate::cmd::fleet::review::schema::Mode::Auto
        }
    )));
}

/// AC15c: while another owner holds the run lock, resume refuses with
/// "running" — even though the channel looks paused.
#[test]
fn resume_refuses_while_the_run_lock_is_held() {
    let (tmp, name) = paused_in_round_two();
    let home = tmp.path();
    let svc = mur_channel::ChannelService::open(home).unwrap();
    let _held =
        crate::cmd::fleet::review::run_lock::try_acquire(&svc, &format!("fleet-{name}")).unwrap();
    let err = prepare_resume(home, &name).unwrap_err().to_string();
    assert!(err.contains("is running"), "{err}");
}

/// AC15e: `fleet delete` on a paused review session appends
/// `session_stopped` (reason `deleted`), keeps the channel, and the next
/// default prune removes it. With the lock held it refuses.
#[test]
fn fleet_delete_on_a_review_session_records_stopped_and_prune_takes_it() {
    use crate::cmd::fleet::review::constants::REVIEW_STOP_REASON_DELETED;
    let (tmp, name) = paused_in_round_two();
    let home = tmp.path();
    let channel = format!("fleet-{name}");
    let svc = mur_channel::ChannelService::open(home).unwrap();

    let held = crate::cmd::fleet::review::run_lock::try_acquire(&svc, &channel).unwrap();
    let err = crate::cmd::fleet::delete::cmd_fleet_delete(home, &name, true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("is running"), "{err}");
    assert!(store::fleet_path(home, &name).exists());
    drop(held);

    crate::cmd::fleet::delete::cmd_fleet_delete(home, &name, true).unwrap();
    assert!(!store::fleet_path(home, &name).exists());
    let all = payloads(home, &channel);
    assert!(matches!(
        all.last(),
        Some(ReviewPayload::SessionStopped { reason, .. }) if reason == REVIEW_STOP_REASON_DELETED
    ));

    let mut out = Vec::new();
    let later = chrono::Utc::now() + chrono::Duration::days(2);
    crate::cmd::fleet::review::prune::prune_reviews(home, "1d", false, false, &mut out, later)
        .unwrap();
    let report = String::from_utf8(out).unwrap();
    assert!(
        report.contains(&format!("pruned {name} (stopped")),
        "{report}"
    );
}

/// AC15f: damaged channel → refusal names the line and the delete exit →
/// following it, default prune past the cutoff removes the session.
#[test]
fn a_damaged_channel_has_a_working_exit() {
    let (tmp, name) = paused_in_round_two();
    let home = tmp.path();
    let channel = format!("fleet-{name}");
    let path = mur_channel::ChannelService::open(home)
        .unwrap()
        .store()
        .events_path(&channel);
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("{truncated\n");
    std::fs::write(&path, text).unwrap();

    let err = prepare_resume(home, &name).unwrap_err().to_string();
    assert!(err.contains("Channel damaged at line"), "{err}");
    assert!(
        err.contains(&format!("Remove it with: mur fleet delete {name}")),
        "{err}"
    );

    crate::cmd::fleet::delete::cmd_fleet_delete(home, &name, true).unwrap();
    let mut out = Vec::new();
    let later = chrono::Utc::now() + chrono::Duration::days(2);
    crate::cmd::fleet::review::prune::prune_reviews(home, "1d", false, false, &mut out, later)
        .unwrap();
    let report = String::from_utf8(out).unwrap();
    assert!(report.contains(&format!("pruned {name}")), "{report}");
    assert!(!path.parent().unwrap().exists());
}

/// AC4a on replay: a running segment's recorded human-input wait is taken
/// out of the execution time rebuilt from the channel.
#[test]
fn replayed_active_time_excludes_human_input_wait() {
    use crate::cmd::fleet::review::schema::Role;
    let t0 = chrono::Utc::now();
    let at = |s: i64| t0 + chrono::Duration::seconds(s);
    let events = vec![
        (
            at(0),
            ReviewPayload::SessionStarted {
                members: ["main".into(), "reviewer".into()],
                mode: crate::cmd::fleet::review::schema::Mode::SemiAuto,
                limits: limits(),
            },
        ),
        (
            at(100),
            ReviewPayload::TurnSent {
                round: 1,
                to: Role::Main,
                restart_note: None,
                human_wait_ms: 60_000,
            },
        ),
    ];
    assert_eq!(super::active_time(&events), Duration::from_secs(40));
}
