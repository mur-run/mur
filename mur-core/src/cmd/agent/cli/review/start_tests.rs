//! Spec §3.1, §3.3, §4.3 / AC-P3b-1, 3, 5, 6: `/review --main … --reviewer …`
//! with nothing attached. Seam: `handle` (the typed line, after `/review `).
//!
//! The worker is the real one over the real A2A edges, but every test stops
//! it at the first send gate (the reply is dropped, so the session pauses as
//! detached) — no agent is ever dialled.

use std::path::Path;
use std::time::Duration;

use tokio::sync::mpsc::Receiver;

use super::handle;
use super::test_fixtures::{agent, app_at, home, stream, system_lines, tx};
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::fleet::review::constants::{REVIEW_AUTO_REFUSED, REVIEW_FLEET_PREFIX};
use crate::cmd::fleet::review::murmur::bridge::{DriverReq, Outcome};

/// Long enough for a loaded CI box; the worker answers in milliseconds.
const WAIT: Duration = Duration::from_secs(20);

fn fleets(home: &Path) -> Vec<String> {
    crate::cmd::fleet::store::list_fleets(home).unwrap()
}

/// The next message, skipping transcript `Show`s.
async fn next(rx: &mut Receiver<StreamMsg>) -> StreamMsg {
    loop {
        let msg = tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("the worker answered in time")
            .expect("the stream stays open until Finished");
        if !matches!(msg, StreamMsg::ReviewReq(DriverReq::Show(_))) {
            return msg;
        }
    }
}

/// Drop the gate's reply (the session pauses), wait for `Finished`, join.
async fn stop_at_gate(app: &mut App, rx: &mut Receiver<StreamMsg>) {
    match next(rx).await {
        StreamMsg::ReviewReq(DriverReq::Confirm { reply, .. }) => drop(reply),
        other => panic!("expected the first send gate, got {other:?}"),
    }
    match next(rx).await {
        StreamMsg::ReviewFinished(Outcome::Err(e)) => panic!("worker failed: {e}"),
        StreamMsg::ReviewFinished(_) => {}
        other => panic!("expected Finished, got {other:?}"),
    }
    let s = app.review.take().expect("attached");
    s.handle.expect("spawned").join.join().unwrap();
}

/// AC-P3b-1: a valid line starts a session — fleet created, worker attached,
/// and its first request (main's send gate) arrives on the UI stream.
#[tokio::test]
async fn start_attaches_a_worker_that_asks_before_main_sends() {
    let tmp = home();
    agent(tmp.path(), "alpha", true);
    agent(tmp.path(), "beta", true);
    let mut app = app_at(tmp.path());
    let (tx, mut rx) = stream();

    handle(&mut app, "--main alpha --reviewer beta fix the bug", &tx).await;

    let s = app.review.as_ref().expect("a session is attached");
    assert!(s.name.starts_with(REVIEW_FLEET_PREFIX), "{}", s.name);
    assert!(s.handle.is_some(), "worker spawned");
    assert_eq!(fleets(tmp.path()), std::slice::from_ref(&s.name));
    let fleet = crate::cmd::fleet::store::load_fleet(tmp.path(), &s.name).unwrap();
    assert_eq!(fleet.members, ["alpha", "beta"]);
    assert_eq!(fleet.goal, "fix the bug");
    let lines = system_lines(&app);
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&s.name) && l.contains("main = alpha")),
        "a start notice names the session and members: {lines:?}"
    );

    match next(&mut rx).await {
        StreamMsg::ReviewReq(DriverReq::Confirm { member, reply, .. }) => {
            assert_eq!(member, "alpha");
            drop(reply);
        }
        other => panic!("expected main's send gate, got {other:?}"),
    }
    match next(&mut rx).await {
        StreamMsg::ReviewFinished(Outcome::Err(e)) => panic!("worker failed: {e}"),
        StreamMsg::ReviewFinished(_) => {}
        other => panic!("expected Finished, got {other:?}"),
    }
    let s = app.review.take().unwrap();
    s.handle.unwrap().join.join().unwrap();
}

/// AC-P3b-3: names typed in another case reach the fleet as
/// `canonicalize_agent_name` resolves them. On a case-insensitive disk that
/// is the typed form; on a case-sensitive one (CI) it is the directory name,
/// and `require_running` would refuse the typed form.
#[tokio::test]
async fn start_canonicalizes_agent_names() {
    let tmp = home();
    agent(tmp.path(), "alpha", true);
    agent(tmp.path(), "beta", true);
    let mut app = app_at(tmp.path());
    let (tx, mut rx) = stream();

    handle(&mut app, "--main ALPHA --reviewer Beta go", &tx).await;

    let canon = |n| crate::a2a_dial::canonicalize_agent_name(tmp.path(), n);
    let name = app.review.as_ref().expect("started").name.clone();
    let fleet = crate::cmd::fleet::store::load_fleet(tmp.path(), &name).unwrap();
    assert_eq!(fleet.members, [canon("ALPHA"), canon("Beta")]);
    stop_at_gate(&mut app, &mut rx).await;
}

/// AC-P3b-5 (syntax): the existing syntax text, the typed line stays, nothing starts.
#[tokio::test]
async fn syntax_error_keeps_the_line_and_starts_nothing() {
    let tmp = home();
    let mut app = app_at(tmp.path());

    handle(&mut app, "--main alpha fix it", &tx()).await;

    let lines = system_lines(&app);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("--reviewer"), "{}", lines[0]);
    assert_eq!(app.input_text(), "/review --main alpha fix it");
    assert!(app.review.is_none());
    assert!(fleets(tmp.path()).is_empty());
}

/// AC-P3b-5 (state): `validate_pair`'s text verbatim, the line stays, nothing starts.
#[tokio::test]
async fn same_agent_twice_is_refused_before_anything_exists() {
    let tmp = home();
    agent(tmp.path(), "alpha", true);
    let mut app = app_at(tmp.path());

    handle(&mut app, "--main alpha --reviewer alpha fix it", &tx()).await;

    let lines = system_lines(&app);
    assert_eq!(
        lines,
        ["--main and --reviewer must be different agents (got 'alpha' for both)"]
    );
    assert_eq!(
        app.input_text(),
        "/review --main alpha --reviewer alpha fix it"
    );
    assert!(app.review.is_none());
    assert!(fleets(tmp.path()).is_empty());
}

/// AC-P3b-5 (state): a member that is down is named with its start command.
#[tokio::test]
async fn a_member_not_running_is_refused_before_anything_exists() {
    let tmp = home();
    agent(tmp.path(), "alpha", true);
    agent(tmp.path(), "beta", false);
    let mut app = app_at(tmp.path());

    handle(&mut app, "--main alpha --reviewer beta fix it", &tx()).await;

    let lines = system_lines(&app);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("mur agent start beta"), "{}", lines[0]);
    assert_eq!(
        app.input_text(),
        "/review --main alpha --reviewer beta fix it"
    );
    assert!(app.review.is_none());
    assert!(fleets(tmp.path()).is_empty());
}

/// AC-P3b-6: `--auto` gets the fixed refusal and nothing starts.
#[tokio::test]
async fn auto_is_refused_with_the_fixed_text() {
    let tmp = home();
    agent(tmp.path(), "alpha", true);
    agent(tmp.path(), "beta", true);
    let mut app = app_at(tmp.path());

    handle(
        &mut app,
        "--auto --main alpha --reviewer beta fix it",
        &tx(),
    )
    .await;

    assert_eq!(system_lines(&app), [REVIEW_AUTO_REFUSED]);
    assert_eq!(
        app.input_text(),
        "/review --auto --main alpha --reviewer beta fix it"
    );
    assert!(app.review.is_none());
    assert!(fleets(tmp.path()).is_empty());
}
