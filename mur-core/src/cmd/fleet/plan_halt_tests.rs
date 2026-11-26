//! #2502: the kill-switch during router planning. The router is a fake
//! runtime on a real unix socket that holds `message/send` open, as the
//! issue's fake Ollama did, so only the stop poll can end the dial.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mur_common::fleet::{Fleet, Job, JobStatus};
use serde_json::{Value, json};

use super::super::loop_run::LoopStop;
use super::super::plan::{PlanHalt, plan_via_router_metered};
use super::*;
use crate::executor::dag::StopSignal;

/// Generous over the 1 s poll, far under the dial's own idle timeout.
const STOP_BOUND: Duration = Duration::from_secs(5);

fn fleet() -> Fleet {
    serde_yaml::from_str(
        "name: vstop\nchannel_id: fleet-vstop\nmembers: [vpm, vqa]\nrouter: vpm\ngoal: g\n",
    )
    .unwrap()
}

/// Every request the fake router saw, as `(method, params)`.
type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// Register a running `name` whose `message/send` never answers (the
/// connection is held until the client hangs up) and whose `tasks/cancel`
/// is answered by `cancel_ok`.
fn hung_router(home: &Path, name: &str, cancel_ok: bool) -> Seen {
    let sock = home.join(format!("{name}.sock"));
    let listener = UnixListener::bind(&sock).unwrap();
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("running.lock"),
        json!({
            "schema": 1, "uuid": format!("u-{name}"), "name": name, "pid": 1, "ppid": 1,
            "started_at": "t", "binary_version": "test", "proto_version": 1,
            "transports": { "stdio": true, "unix_socket": sock.to_str().unwrap() },
            "card_digest": "d", "capabilities": [],
        })
        .to_string(),
    )
    .unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let log = log.clone();
            std::thread::spawn(move || {
                let mut w = conn.try_clone().unwrap();
                let mut reader = BufReader::new(conn);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                let method = req["method"].as_str().unwrap_or("").to_string();
                log.lock()
                    .unwrap()
                    .push((method.clone(), req["params"].clone()));
                match method.as_str() {
                    // Hold the planning turn open: wait for the hang-up.
                    "message/send" => {
                        let mut rest = String::new();
                        let _ = reader.read_line(&mut rest);
                    }
                    "tasks/cancel" => {
                        let reply = if cancel_ok {
                            json!({"jsonrpc": "2.0", "id": req["id"], "result": {"state": "canceled"}})
                        } else {
                            json!({"jsonrpc": "2.0", "id": req["id"],
                                "error": {"code": -32000, "message": "refused"}})
                        };
                        let _ = writeln!(w, "{reply}");
                    }
                    _ => {}
                }
            });
        }
    });
    seen
}

/// A stop signal that trips once `after` has passed.
fn stop_after(after: Duration) -> StopSignal {
    let at = Instant::now() + after;
    Arc::new(move || Instant::now() >= at)
}

/// Run the planning dial off the test thread, so a regression (the dial
/// never ending) fails the bound instead of hanging the suite.
fn plan_bounded(
    home: &Path,
    stop: StopSignal,
    bound: Duration,
) -> Option<super::super::plan::RouterTurn> {
    let home = home.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let turn = plan_via_router_metered(&home, &fleet(), "g", &[], Some(&stop));
        let _ = tx.send(turn);
    });
    rx.recv_timeout(bound).ok()
}

/// The issue's scenario: the router has sent nothing yet when the stop
/// lands. The dial ends within the bound, the router is asked to cancel the
/// very id this side sent, and no plan comes back to broadcast.
#[test]
fn a_stop_during_planning_ends_the_dial_and_cancels_the_routers_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let seen = hung_router(tmp.path(), "vpm", true);
    let started = Instant::now();
    let turn = plan_bounded(
        tmp.path(),
        stop_after(Duration::from_millis(200)),
        STOP_BOUND,
    )
    .expect("planning must end once the kill-switch trips");
    assert!(started.elapsed() < STOP_BOUND);
    assert!(
        matches!(turn.halt, Some(PlanHalt::Stopped)),
        "{:?}",
        turn.halt
    );
    assert!(turn.plan.is_none());

    let seen = seen.lock().unwrap();
    let sent = seen
        .iter()
        .find(|(m, _)| m == "message/send")
        .expect("planning sent");
    let cancel = seen
        .iter()
        .find(|(m, _)| m == "tasks/cancel")
        .expect("cancel sent");
    let id = sent.1["task_id"]
        .as_str()
        .expect("planning carries its own id");
    assert!(id.starts_with("plan-"), "{id}");
    assert_eq!(cancel.1["id"], id, "cancel must name the planning turn");
}

/// A refused cancel is not a clean stop: the router may still be spending.
#[test]
fn a_refused_router_cancel_is_reported_not_swallowed() {
    let tmp = tempfile::tempdir().unwrap();
    hung_router(tmp.path(), "vpm", false);
    let turn = plan_bounded(
        tmp.path(),
        stop_after(Duration::from_millis(200)),
        STOP_BOUND,
    )
    .expect("planning must end once the kill-switch trips");
    assert!(
        matches!(turn.halt, Some(PlanHalt::CancelRefused(_))),
        "{:?}",
        turn.halt
    );
}

/// With no stop, nothing is cancelled: the poll only reads the signal.
#[test]
fn an_untripped_signal_cancels_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let seen = hung_router(tmp.path(), "vpm", true);
    // Never trips; the hung router keeps the dial open past two polls.
    let two_polls = Duration::from_millis(2_500);
    assert!(plan_bounded(tmp.path(), Arc::new(|| false), two_polls).is_none());
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|(m, _)| m == "tasks/cancel"),
        "an untripped signal must not cancel the router"
    );
}

fn running_job() -> Job {
    Job {
        id: "j1".into(),
        text: "t".into(),
        source: "cli".into(),
        status: JobStatus::Running,
        created_at: "t0".into(),
        started_at: Some("t0".into()),
        finished_at: None,
        run_id: None,
        result: None,
        error: None,
    }
}

#[test]
fn a_clean_planning_stop_cancels_the_job_and_stops_the_loop() {
    let mut job = running_job();
    let stop = settle(PlanHalt::Stopped, Some(&mut job), "run-1");
    assert_eq!(stop, LoopStop::Stopped);
    assert_eq!(job.status, JobStatus::Canceled);
    assert_eq!(job.run_id.as_deref(), Some("run-1"));
    assert!(job.finished_at.is_some());
    assert_eq!(job.error.as_deref(), Some(STOPPED_TEXT));
}

#[test]
fn a_refused_planning_cancel_fails_the_job_and_the_loop() {
    let mut job = running_job();
    let stop = settle(
        PlanHalt::CancelRefused(anyhow::anyhow!("refused")),
        Some(&mut job),
        "",
    );
    assert_eq!(stop, LoopStop::IterationFailed);
    assert_eq!(job.status, JobStatus::Failed);
    // No run id yet (one-shot planning precedes it): none is invented.
    assert!(job.run_id.is_none());
    let err = job.error.unwrap();
    assert!(
        err.starts_with(REFUSED_TEXT) && err.ends_with("refused"),
        "{err}"
    );
}
