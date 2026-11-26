//! #2590: kill-switch paths the #2503 tests left uncovered.
//!
//! Each test stands up fake members on unix sockets, so all of them are gated
//! behind `MUR_TEST_SOCKETS=1` like the parent module's socket tests. The stop
//! is tripped by the fake member itself the moment it receives
//! `channel/delegate`, never by a timer: the stop is guaranteed to land while
//! the delegation is in flight, however slow the runner.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

/// How long a fake member holds a turn open before giving up on its own, so a
/// broken test fails instead of hanging CI.
const TURN_CEILING: Duration = Duration::from_secs(20);

fn sockets_enabled(test: &str) -> bool {
    let on = std::env::var("MUR_TEST_SOCKETS").as_deref() == Ok("1");
    if !on {
        eprintln!("skipping {test}: set MUR_TEST_SOCKETS=1");
    }
    on
}

/// A one-way latch a fake member's threads wait on.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn open(&self) {
        let (lock, cv) = &*self.0;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    fn wait(&self) {
        let (lock, cv) = &*self.0;
        let g = lock.lock().unwrap();
        let _ = cv.wait_timeout_while(g, TURN_CEILING, |o| !*o).unwrap();
    }
}

fn ok(req: &serde_json::Value, result: serde_json::Value) -> Option<serde_json::Value> {
    Some(serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":result}))
}

fn rpc_err(req: &serde_json::Value, message: &str) -> Option<serde_json::Value> {
    Some(serde_json::json!({"jsonrpc":"2.0","id":req["id"],
        "error":{"code":-32000,"message":message}}))
}

/// The delegate's answer once its turn was cancelled.
fn canceled_task(req: &serde_json::Value) -> Option<serde_json::Value> {
    ok(
        req,
        serde_json::json!({ "id": req["params"]["task_id"], "status": { "state": "canceled" } }),
    )
}

/// A `tasks/get` answer for an ended turn that reports its usage, so a failed
/// step's late-usage recovery returns at once instead of waiting out its grace.
fn ended_with_usage(req: &serde_json::Value) -> Option<serde_json::Value> {
    ok(
        req,
        serde_json::json!({ "id": req["params"]["id"], "state": "canceled",
            "usage": { "input_tokens": 1, "output_tokens": 1 } }),
    )
}

/// Register a running member `name` on a unix socket under `home`. Every
/// request is answered by `handler` on its own thread (so it may block);
/// `None` drops the connection without a reply — a transport failure.
fn fake_member<H>(home: &Path, name: &str, handler: H)
where
    H: Fn(&str, &serde_json::Value) -> Option<serde_json::Value> + Send + Sync + 'static,
{
    let sock = home.join(format!("{name}.sock"));
    let listener = UnixListener::bind(&sock).unwrap();
    let agent_dir = home.join("agents").join(name);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("running.lock"),
        serde_json::json!({
            "schema": 1, "uuid": format!("u-{name}"), "name": name, "pid": 1, "ppid": 1,
            "started_at": "t", "binary_version": "test", "proto_version": 1,
            "transports": { "stdio": true, "unix_socket": sock.to_str().unwrap() },
            "card_digest": "d", "capabilities": [],
        })
        .to_string(),
    )
    .unwrap();
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let handler = handler.clone();
            std::thread::spawn(move || {
                let mut w = conn.try_clone().unwrap();
                let mut line = String::new();
                if BufReader::new(conn).read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                let method = req["method"].as_str().unwrap_or("").to_string();
                if let Some(reply) = handler(&method, &req) {
                    let _ = writeln!(w, "{reply}");
                }
            });
        }
    });
}

type Events = Arc<Mutex<Vec<(String, StepEventKind, Option<String>)>>>;

/// Run `steps` (all delegations) under a stop signal the fake members trip,
/// and return the run's status plus every step event as (id, kind, error).
async fn run_stoppable(
    home: &Path,
    steps: Vec<ProcedureStep>,
    stop: StopSignal,
) -> (PipelineStatus, Vec<(String, StepEventKind, Option<String>)>) {
    let svc = ChannelService::open(home).unwrap();
    let ch = svc
        .create_for_workflow("t", mur_channel::service::ChannelOwner::Legacy)
        .unwrap();
    let events: Events = Arc::default();
    let on_step: Arc<dyn Fn(StepEvent) + Send + Sync> = {
        let events = events.clone();
        Arc::new(move |e: StepEvent| events.lock().unwrap().push((e.id, e.kind, e.error)))
    };
    let out = execute_dag(
        home,
        "t",
        &Procedure {
            variables: vec![],
            steps,
        },
        &DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "r".into(),
            stop: Some(stop),
            on_step: Some(on_step),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    let events = events.lock().unwrap().clone();
    (out.status, events)
}

fn delegate(id: &str, member: &str) -> ProcedureStep {
    ProcedureStep {
        id: Some(id.into()),
        description: format!("{id} research"),
        delegate_to: Some(member.into()),
        ..Default::default()
    }
}

fn stop_flag() -> (Arc<AtomicBool>, StopSignal) {
    let f = Arc::new(AtomicBool::new(false));
    let g = f.clone();
    (f, Arc::new(move || g.load(Ordering::SeqCst)))
}

/// #2590 path 1: the `tasks/cancel` never reaches the member (the connection
/// drops before any reply) — a transport failure, not an RPC refusal. Nothing
/// confirms the member stopped, so the step fails and the run is not Stopped.
#[tokio::test]
async fn a_cancel_that_never_reaches_the_member_is_not_reported_as_stopped() {
    if !sockets_enabled("a_cancel_that_never_reaches_the_member") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let (f, stop) = stop_flag();
    let released = Gate::default();
    fake_member(home, "member", move |method, req| match method {
        "channel/delegate" => {
            f.store(true, Ordering::SeqCst);
            released.wait();
            canceled_task(req)
        }
        "tasks/cancel" => {
            // Let the abandoned delegate dial end, but never answer the cancel.
            released.open();
            None
        }
        "tasks/get" => ended_with_usage(req),
        _ => ok(req, serde_json::json!({})),
    });

    let (status, events) = run_stoppable(home, vec![delegate("s1", "member")], stop).await;

    assert_eq!(status, PipelineStatus::Failed, "events: {events:?}");
    assert!(
        !events.iter().any(|(_, k, _)| *k == StepEventKind::Stopped),
        "an undelivered cancel must not read as a clean stop: {events:?}"
    );
    let (_, _, why) = events
        .iter()
        .find(|(_, k, _)| *k == StepEventKind::Failed)
        .unwrap_or_else(|| panic!("the step must fail: {events:?}"));
    let why = why.clone().unwrap_or_default();
    assert!(why.contains("member"), "error names the member: {why}");
    assert!(
        why.contains("tasks/cancel"),
        "error names the cancel: {why}"
    );
}

/// #2590 path 3: two delegations in one rank; `alpha` refuses the cancel,
/// `beta` accepts it. Each step reports its own outcome, and the run as a
/// whole is Failed — one member may still be running, so it is not Stopped.
#[tokio::test]
async fn a_refusal_beside_an_accepted_cancel_fails_the_run_and_keeps_each_outcome() {
    if !sockets_enabled("a_refusal_beside_an_accepted_cancel") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let (f, stop) = stop_flag();
    // Trip the stop only once BOTH delegations are in flight, so neither step
    // can take the "stopped before dialing" path.
    let in_flight = Arc::new(AtomicUsize::new(0));
    for (name, refuse) in [("alpha", true), ("beta", false)] {
        let (f, in_flight, released) = (f.clone(), in_flight.clone(), Gate::default());
        fake_member(home, name, move |method, req| match method {
            "channel/delegate" => {
                if in_flight.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                    f.store(true, Ordering::SeqCst);
                }
                released.wait();
                canceled_task(req)
            }
            "tasks/cancel" => {
                released.open();
                if refuse {
                    rpc_err(req, "cancel refused")
                } else {
                    ok(req, serde_json::json!({}))
                }
            }
            "tasks/get" => ended_with_usage(req),
            _ => ok(req, serde_json::json!({})),
        });
    }

    let (status, events) = run_stoppable(
        home,
        vec![delegate("a", "alpha"), delegate("b", "beta")],
        stop,
    )
    .await;

    assert_eq!(
        status,
        PipelineStatus::Failed,
        "a refused cancel anywhere fails the run: {events:?}"
    );
    let final_kind = |id: &str| {
        events
            .iter()
            .filter(|(i, k, _)| i == id && *k != StepEventKind::Started)
            .map(|(_, k, e)| (*k, e.clone()))
            .next_back()
            .unwrap_or_else(|| panic!("no final event for step {id}: {events:?}"))
    };
    let (a, why) = final_kind("a");
    assert_eq!(a, StepEventKind::Failed, "alpha refused: {events:?}");
    let why = why.unwrap_or_default();
    assert!(
        why.contains("alpha") && why.contains("tasks/cancel"),
        "{why}"
    );
    let (b, _) = final_kind("b");
    assert_eq!(b, StepEventKind::Stopped, "beta accepted: {events:?}");
}

/// #2590 path 2: the blocking dial itself fails (the member's connection
/// drops) after the stop tripped but before the poll noticed it, so no
/// kill-switch cancel was sent. The member's turn is still running and it
/// refuses the cancel the failure path sends: nothing stopped it, so the run
/// must not read as a clean stop.
#[tokio::test]
async fn a_dial_failure_during_a_stop_with_the_turn_still_running_is_not_stopped() {
    if !sockets_enabled("a_dial_failure_during_a_stop") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let (f, stop) = stop_flag();
    fake_member(home, "member", move |method, req| match method {
        "channel/delegate" => {
            // Trip the stop and drop the connection at once: the dial errors
            // well inside one STOP_POLL_INTERVAL.
            f.store(true, Ordering::SeqCst);
            None
        }
        "tasks/get" => ok(
            req,
            serde_json::json!({ "id": req["params"]["id"], "state": "working" }),
        ),
        "tasks/cancel" => rpc_err(req, "cancel refused"),
        _ => ok(req, serde_json::json!({})),
    });

    let svc = ChannelService::open(home).unwrap();
    let ch = svc
        .create_for_workflow("t", mur_channel::service::ChannelOwner::Legacy)
        .unwrap();
    let events: Events = Arc::default();
    let on_step: Arc<dyn Fn(StepEvent) + Send + Sync> = {
        let events = events.clone();
        Arc::new(move |e: StepEvent| events.lock().unwrap().push((e.id, e.kind, e.error)))
    };
    let out = execute_dag(
        home,
        "t",
        &Procedure {
            variables: vec![],
            steps: vec![delegate("s1", "member")],
        },
        &DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "r".into(),
            stop: Some(stop),
            on_step: Some(on_step),
            // Bound the late-usage wait so a still-running turn costs seconds.
            deadline_at: Some(std::time::Instant::now() + Duration::from_secs(4)),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    let events = events.lock().unwrap().clone();
    assert_ne!(
        out.status,
        PipelineStatus::Stopped,
        "the member refused the cancel and is still running: {events:?}"
    );
    assert!(
        !events.iter().any(|(_, k, _)| *k == StepEventKind::Stopped),
        "{events:?}"
    );
}

/// #2590 path 2, unreachable variant: the dial fails after the stop tripped
/// and the member then answers nothing at all — late-usage recovery's
/// `tasks/get` fails before any cancel is sent. Nothing shows the turn
/// stopped, so the dial's error stands: the step fails and so does the run.
#[tokio::test]
async fn a_dial_failure_during_a_stop_with_the_member_unreachable_fails_the_run() {
    if !sockets_enabled("a_dial_failure_during_a_stop_with_the_member_unreachable") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let (f, stop) = stop_flag();
    fake_member(home, "member", move |method, _| {
        if method == "channel/delegate" {
            f.store(true, Ordering::SeqCst);
        }
        // Every connection drops unanswered: the dial and the probe both fail.
        None
    });

    let (status, events) = run_stoppable(home, vec![delegate("s1", "member")], stop).await;

    assert_eq!(status, PipelineStatus::Failed, "events: {events:?}");
    assert!(
        events
            .iter()
            .any(|(id, k, _)| id == "s1" && *k == StepEventKind::Failed),
        "the step must fail: {events:?}"
    );
    assert!(
        !events.iter().any(|(_, k, _)| *k == StepEventKind::Stopped),
        "{events:?}"
    );
}

/// The counterpart: the dial fails after the stop tripped, but the member
/// reports the turn ended. Nothing is left running, so the failure is the
/// stop's doing and the run still ends `Stopped`.
#[tokio::test]
async fn a_dial_failure_during_a_stop_with_the_turn_ended_stays_stopped() {
    if !sockets_enabled("a_dial_failure_during_a_stop_with_the_turn_ended") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let (f, stop) = stop_flag();
    fake_member(home, "member", move |method, req| match method {
        "channel/delegate" => {
            f.store(true, Ordering::SeqCst);
            None
        }
        "tasks/get" => ended_with_usage(req),
        _ => ok(req, serde_json::json!({})),
    });

    let (status, events) = run_stoppable(home, vec![delegate("s1", "member")], stop).await;

    assert_eq!(status, PipelineStatus::Stopped, "events: {events:?}");
}
