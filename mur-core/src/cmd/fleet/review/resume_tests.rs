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
    assert!(
        err.contains("not paused") || err.contains("session_started"),
        "{err}"
    );
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
