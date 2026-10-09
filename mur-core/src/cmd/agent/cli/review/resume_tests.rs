//! Spec §3.4, §8 / AC-P3b-4b, 5, 7: bare `/review` and `/review resume <n>`
//! with nothing attached. Seams: `handle` (the typed line after `/review `)
//! and `submit` (the line typed at `Paused — continue?`).
//!
//! Sessions are written straight onto the channel (`fleet::review::state`
//! fixtures); a resumed worker is the real one, stopped at its first request.

use std::path::Path;
use std::time::Duration;

use mur_channel::ChannelService;
use mur_common::limits::Stuck;

use super::ReviewSession;
use super::handle;
use super::state::Awaiting;
use super::test_fixtures::{agent, app_at, home, next, stream, system_lines, tx};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::agent::cli::turn::submit;
use crate::cmd::fleet::review::constants::{
    REVIEW_LEFT_PAUSED_NOTICE, REVIEW_NO_PAUSED, REVIEW_PAUSED_CONTINUE_PROMPT,
    REVIEW_USAGE_MURMUR, RUNNING_LOCK,
};
use crate::cmd::fleet::review::murmur::bridge::{DriverReq, Outcome};
use crate::cmd::fleet::review::resume::prepare_resume;
use crate::cmd::fleet::review::run_lock::try_acquire;
use crate::cmd::fleet::review::schema::{
    Cumulative, Mode, PauseKind, RebuttalAnswer, RebuttalResponseDto, ReviewPayload, Role,
    SessionLimits, Severity, VerdictKind,
};
use crate::cmd::fleet::review::state::state_tests::{mid_round, paused, session};

const PAUSED: &str = "review-pa110001";
const CRASHED: &str = "review-cr220002";
const HELD: &str = "review-he330003";

fn cum(n: u64) -> Cumulative {
    Cumulative {
        exec_time_ms: n * 100,
        cost_usd_micros: n,
    }
}

fn started() -> ReviewPayload {
    ReviewPayload::SessionStarted {
        members: ["main".into(), "reviewer".into()],
        mode: Mode::SemiAuto,
        limits: SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
    }
}

fn sent(round: u32, to: Role) -> ReviewPayload {
    ReviewPayload::TurnSent {
        round,
        to,
        restart_note: None,
        human_wait_ms: 0,
    }
}

fn verdict(round: u32) -> ReviewPayload {
    ReviewPayload::Verdict {
        round,
        kind: VerdictKind::Revise,
        cumulative: cum(round.into()),
    }
}

fn reject_f1(round: u32) -> ReviewPayload {
    ReviewPayload::Rebuttal {
        round,
        responses: vec![RebuttalResponseDto {
            id: "F1".into(),
            answer: RebuttalAnswer::Reject,
            reason: Some("disagree".into()),
        }],
        cumulative: cum(round.into()),
    }
}

fn paused_as(kind: PauseKind, round: u32) -> ReviewPayload {
    ReviewPayload::Paused {
        kind,
        reason: "detached".into(),
        cumulative: cum(round.into()),
        human_wait_ms: 0,
    }
}

/// Round 1 sealed, then paused: resumable, nothing owed.
fn resumable(home: &Path, name: &str) -> String {
    agent(home, "main", true);
    agent(home, "reviewer", true);
    session(
        home,
        name,
        &[
            started(),
            sent(1, Role::Main),
            sent(1, Role::Reviewer),
            verdict(1),
            paused_as(PauseKind::Detached, 1),
        ],
        true,
    )
}

/// F1 issued in round 1, rejected in rounds 2 and 3 (escalates at the
/// round-3 seal), paused on the escalation: resuming owes a ruling.
fn owes_ruling(home: &Path, name: &str) -> String {
    agent(home, "main", true);
    agent(home, "reviewer", true);
    let mut log = vec![
        started(),
        sent(1, Role::Main),
        sent(1, Role::Reviewer),
        ReviewPayload::FindingIssued {
            round: 1,
            id: "F1".into(),
            severity: Severity::High,
            issue: "x".into(),
        },
        verdict(1),
    ];
    for round in [2, 3] {
        log.extend([
            sent(round, Role::Main),
            sent(round, Role::Reviewer),
            reject_f1(round),
            verdict(round),
        ]);
    }
    log.push(paused_as(PauseKind::Escalation, 3));
    session(home, name, &log, true)
}

fn lock_is_free(home: &Path, channel_id: &str) -> bool {
    let svc = ChannelService::open(home).unwrap();
    try_acquire(&svc, channel_id).is_ok()
}

fn attached(app: &App) -> &ReviewSession {
    app.review.as_ref().expect("a session is attached")
}

async fn type_line(app: &mut App, line: &str) {
    app.set_input(line);
    submit(app, &tx()).await;
}

// ── bare `/review` ────────────────────────────────────────────────────────

/// §3.1 / §3.4: usage, then paused and crashed rows, each with the resume
/// command; nothing attaches.
#[tokio::test]
async fn bare_lists_paused_and_crashed_with_how_to_resume() {
    let tmp = home();
    let h = tmp.path();
    session(h, PAUSED, &[paused()], true);
    session(h, CRASHED, &[mid_round()], true);
    let mut app = app_at(h);

    handle(&mut app, "", &tx()).await;

    let text = system_lines(&app).join("\n");
    assert!(text.contains(REVIEW_USAGE_MURMUR), "{text}");
    for (name, state) in [(PAUSED, "paused"), (CRASHED, "crashed")] {
        let row = text
            .lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("no row for {name}: {text}"));
        assert!(row.contains(state), "{row}");
        assert!(row.contains(&format!("/review resume {name}")), "{row}");
    }
    assert!(app.review.is_none());
}

#[tokio::test]
async fn bare_with_nothing_paused_says_so() {
    let tmp = home();
    let mut app = app_at(tmp.path());

    handle(&mut app, "", &tx()).await;

    let text = system_lines(&app).join("\n");
    assert!(text.contains(REVIEW_USAGE_MURMUR), "{text}");
    assert!(text.contains(REVIEW_NO_PAUSED), "{text}");
}

/// AC-P3b-4b: a session another process holds is listed as `running`, is
/// not offered for resume, and keeps its lock.
#[tokio::test]
async fn bare_lists_a_session_held_elsewhere_as_running_and_not_offered() {
    let tmp = home();
    let h = tmp.path();
    let ch = session(h, HELD, &[paused()], true);
    let svc = ChannelService::open(h).unwrap();
    let _held = try_acquire(&svc, &ch).unwrap();
    let mut app = app_at(h);

    handle(&mut app, "", &tx()).await;

    let text = system_lines(&app).join("\n");
    let row = text.lines().find(|l| l.starts_with(HELD)).expect(&text);
    assert!(row.contains("running"), "{row}");
    assert!(!text.contains(&format!("/review resume {HELD}")), "{text}");
    assert!(try_acquire(&svc, &ch).is_err(), "holder keeps the lock");
}

// ── `/review resume <n>` ──────────────────────────────────────────────────

/// §3.3 / AC-P3b-5: `prepare_resume`'s text verbatim, the line stays, no worker.
#[tokio::test]
async fn resume_of_a_missing_session_is_refused_verbatim() {
    let tmp = home();
    let h = tmp.path();
    let mut app = app_at(h);
    let want = format!("{:#}", prepare_resume(h, "review-nope0001").unwrap_err());

    handle(&mut app, "resume review-nope0001", &tx()).await;

    assert_eq!(system_lines(&app), [want.as_str()]);
    assert_eq!(app.input_text(), "/review resume review-nope0001");
    assert!(app.review.is_none());
}

/// AC-P3b-7: another process holds the lock → its existing refusal (with the
/// holder's description), no worker, and the holder keeps the lock.
#[tokio::test]
async fn resume_of_a_session_held_elsewhere_keeps_the_holders_lock() {
    let tmp = home();
    let h = tmp.path();
    let ch = resumable(h, HELD);
    let svc = ChannelService::open(h).unwrap();
    let _held = try_acquire(&svc, &ch).unwrap();
    let want = format!("{:#}", prepare_resume(h, HELD).unwrap_err());
    assert!(want.contains("is running (pid"), "{want}");
    let mut app = app_at(h);

    handle(&mut app, &format!("resume {HELD}"), &tx()).await;

    assert_eq!(system_lines(&app), [want.as_str()]);
    assert_eq!(app.input_text(), format!("/review resume {HELD}"));
    assert!(app.review.is_none());
    assert!(try_acquire(&svc, &ch).is_err(), "holder keeps the lock");
}

/// A member that is down is refused like `mur fleet review-resume` does, and
/// the lock `prepare_resume` took is released again.
#[tokio::test]
async fn resume_with_a_member_down_is_refused_and_releases_the_lock() {
    let tmp = home();
    let h = tmp.path();
    let ch = resumable(h, PAUSED);
    let down = h.join("agents/reviewer").join(RUNNING_LOCK);
    std::fs::remove_file(down).unwrap();
    let mut app = app_at(h);

    handle(&mut app, &format!("resume {PAUSED}"), &tx()).await;

    let lines = system_lines(&app);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("mur agent start reviewer"),
        "{}",
        lines[0]
    );
    assert_eq!(app.input_text(), format!("/review resume {PAUSED}"));
    assert!(app.review.is_none());
    assert!(lock_is_free(h, &ch));
}

/// §8 steps 2–4: summary and `Paused — continue?` as system text, the lock
/// held by the attached session, no worker yet.
#[tokio::test]
async fn resume_shows_the_summary_and_waits_holding_the_lock() {
    let tmp = home();
    let h = tmp.path();
    let ch = resumable(h, PAUSED);
    let mut app = app_at(h);

    handle(&mut app, &format!("resume {PAUSED}"), &tx()).await;

    let text = system_lines(&app).join("\n");
    // Round 1 is sealed, so the session resumes at round 2.
    assert!(text.contains("Paused at round 2"), "{text}");
    assert!(
        text.contains(REVIEW_PAUSED_CONTINUE_PROMPT.trim_end()),
        "{text}"
    );
    let s = attached(&app);
    assert_eq!(s.name, PAUSED);
    assert_eq!(s.channel_id, ch);
    assert!(s.handle.is_none(), "no worker before the answer");
    assert!(matches!(s.awaiting, Some(Awaiting::ResumeConfirm(_))));
    assert_eq!(s.esc, ReviewEsc::Detached);
    assert!(!lock_is_free(h, &ch), "the waiting session holds the lock");
    assert_eq!(app.input_text(), "");
}

/// §8 step 4: anything but a send answer leaves it paused and frees the lock.
#[tokio::test]
async fn a_non_answer_at_continue_leaves_it_paused_and_releases_the_lock() {
    let tmp = home();
    let h = tmp.path();
    let ch = resumable(h, PAUSED);
    let mut app = app_at(h);
    handle(&mut app, &format!("resume {PAUSED}"), &tx()).await;

    type_line(&mut app, "nope").await;

    assert!(app.review.is_none());
    assert_eq!(system_lines(&app).last(), Some(&REVIEW_LEFT_PAUSED_NOTICE));
    assert!(lock_is_free(h, &ch));
    assert_eq!(app.input_text(), "");
}

/// §8 step 4: Enter starts the worker with the held lock; its first request
/// is main's send gate (semi-auto).
#[tokio::test]
async fn enter_at_continue_starts_the_worker() {
    let tmp = home();
    let h = tmp.path();
    resumable(h, PAUSED);
    let mut app = app_at(h);
    let (tx, mut rx) = stream();
    handle(&mut app, &format!("resume {PAUSED}"), &tx).await;

    app.set_input("");
    submit(&mut app, &tx).await;

    let s = attached(&app);
    assert!(s.awaiting.is_none());
    assert!(s.handle.is_some(), "worker spawned");
    match next(&mut rx).await {
        StreamMsg::ReviewReq(DriverReq::Confirm { member, reply, .. }) => {
            assert_eq!(member, "main");
            drop(reply);
        }
        other => panic!("expected main's send gate, got {other:?}"),
    }
    finish(&mut app, &mut rx).await;
}

/// §8 step 5: a session that owes a ruling skips the continue question and
/// starts the worker straight into the ruling prompt.
#[tokio::test]
async fn resume_owing_a_ruling_starts_at_the_ruling_prompt() {
    let tmp = home();
    let h = tmp.path();
    owes_ruling(h, PAUSED);
    let mut app = app_at(h);
    let (tx, mut rx) = stream();

    handle(&mut app, &format!("resume {PAUSED}"), &tx).await;

    let s = attached(&app);
    assert!(s.awaiting.is_none());
    assert!(s.handle.is_some(), "worker spawned without asking");
    let text = system_lines(&app).join("\n");
    assert!(
        !text.contains(REVIEW_PAUSED_CONTINUE_PROMPT.trim_end()),
        "{text}"
    );
    match next(&mut rx).await {
        StreamMsg::ReviewReq(DriverReq::Ruling { text, reply, .. }) => {
            assert!(text.contains("F1"), "{text}");
            drop(reply); // a gone UI reads as `q`: left paused
        }
        other => panic!("expected the ruling prompt, got {other:?}"),
    }
    finish(&mut app, &mut rx).await;
}

async fn finish(app: &mut App, rx: &mut tokio::sync::mpsc::Receiver<StreamMsg>) {
    match next(rx).await {
        StreamMsg::ReviewFinished(Outcome::Err(e)) => panic!("worker failed: {e}"),
        StreamMsg::ReviewFinished(_) => {}
        other => panic!("expected Finished, got {other:?}"),
    }
    let s = app.review.take().expect("attached");
    s.handle.expect("spawned").join.join().unwrap();
}
