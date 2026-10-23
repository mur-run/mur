//! #2036: the kill-switch inside a DAG run.
//!
//! The mid-turn test stands up a fake member on a unix socket, so it is gated
//! behind `MUR_TEST_SOCKETS=1` like the other socket tests (some sandboxes
//! deny `AF_UNIX` connect). The step-boundary tests need no socket.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

fn flag() -> (Arc<AtomicBool>, StopSignal) {
    let f = Arc::new(AtomicBool::new(false));
    let g = f.clone();
    (f, Arc::new(move || g.load(Ordering::SeqCst)))
}

fn echo_step(id: &str, deps: &[&str], cmd: &str) -> ProcedureStep {
    ProcedureStep {
        id: Some(id.to_string()),
        command: Some(cmd.to_string()),
        description: format!("{id} step"),
        depends_on: deps.iter().map(|d| d.to_string()).collect(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_stop_before_the_run_starts_nothing_and_ends_stopped() {
    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("ran");
    let (f, stop) = flag();
    f.store(true, Ordering::SeqCst);
    let proc = Procedure {
        variables: vec![],
        steps: vec![echo_step("s0", &[], &format!("touch {}", marker.display()))],
    };
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            stop: Some(stop),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(
        !marker.exists(),
        "a stopped run must not start its first step"
    );
}

/// The stop lands while rank 0 runs. Rank 1 must never start, and the run
/// must end `Stopped` even though every step that did run succeeded — the
/// shape of the lost-stop bug (a stop during the final iteration read as
/// done).
#[tokio::test]
async fn a_stop_during_a_rank_starts_no_later_rank_and_never_reads_done() {
    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("rank1-ran");
    let (f, stop) = flag();
    let steps = vec![
        echo_step("s0", &[], "sleep 0.3"),
        echo_step("s1", &["s0"], &format!("touch {}", marker.display())),
    ];
    let proc = Procedure {
        variables: vec![],
        steps,
    };
    let flip = {
        let f = f.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            f.store(true, Ordering::SeqCst);
        })
    };
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            stop: Some(stop),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    flip.await.unwrap();
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(!marker.exists(), "rank 1 started after the stop");
}

#[tokio::test]
async fn without_a_stop_signal_the_run_is_unchanged() {
    let tmp = tempfile::TempDir::new().unwrap();
    let proc = Procedure {
        variables: vec![],
        steps: vec![echo_step("s0", &[], "echo hi")],
    };
    let (_f, stop) = flag();
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            stop: Some(stop),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(out.status, PipelineStatus::Success);
}

/// A stopped run writes the channel's `canceled` terminal — the same one the
/// fleet loop writes for `LoopStop::Stopped` — and the run record says
/// `stopped`, not done or failed.
#[tokio::test]
async fn a_stopped_run_is_canceled_in_the_channel_and_stopped_in_the_record() {
    let tmp = tempfile::TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("t").unwrap();
    let (f, stop) = flag();
    f.store(true, Ordering::SeqCst);
    let proc = Procedure {
        variables: vec![],
        steps: vec![echo_step("s0", &[], "echo hi")],
    };
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "run-2036".into(),
            run_kind: Some(crate::run_status::RunKind::Workflow),
            stop: Some(stop),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert_eq!(
        svc.store().load_manifest(&ch.id).unwrap().state,
        ChannelState::Canceled
    );
    let rec = crate::run_status::store::load(tmp.path(), "run-2036")
        .unwrap()
        .expect("run recorded");
    assert_eq!(rec.state, crate::run_status::State::Stopped);
    // Stopped before it began: no step was started, so none may read as
    // running or done.
    assert!(
        rec.steps.iter().all(|s| !matches!(
            s.state,
            crate::run_status::State::Running | crate::run_status::State::Done
        )),
        "{:?}",
        rec.steps
    );
}

/// The core of #2036: a member turn in flight when the operator stops the
/// run is cancelled with `tasks/cancel` for the delegation's own task id, and
/// the step returns at once instead of waiting out the turn.
#[cfg(unix)]
#[tokio::test]
async fn a_stop_mid_turn_cancels_the_member_task_and_returns_promptly() {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping a_stop_mid_turn_cancels_the_member_task: set MUR_TEST_SOCKETS=1");
        return;
    }
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("t").unwrap();

    // Fake member: holds `channel/delegate` open (a long member turn) until
    // `tasks/cancel` arrives, then answers the delegate with a canceled task —
    // what the real runtime does when a `run_sync` turn is cancelled.
    let sock = home.join("m.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let agent_dir = home.join("agents").join("member");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("running.lock"),
        serde_json::json!({
            "schema": 1, "uuid": "u", "name": "member", "pid": 1, "ppid": 1,
            "started_at": "t", "binary_version": "test", "proto_version": 1,
            "transports": { "stdio": true, "unix_socket": sock.to_str().unwrap() },
            "card_digest": "d", "capabilities": [],
        })
        .to_string(),
    )
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<(String, serde_json::Value)>();
    let cancelled = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let tx = tx.clone();
            let cancelled = cancelled.clone();
            std::thread::spawn(move || {
                let mut w = conn.try_clone().unwrap();
                let mut line = String::new();
                if BufReader::new(conn).read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                let method = req["method"].as_str().unwrap_or("").to_string();
                let _ = tx.send((method.clone(), req["params"].clone()));
                let (lock, cv) = &*cancelled;
                if method == "tasks/cancel" {
                    *lock.lock().unwrap() = true;
                    cv.notify_all();
                    let reply = serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":{}});
                    let _ = writeln!(w, "{reply}");
                } else {
                    // A long member turn, ended only by the cancel (or a
                    // generous ceiling so a broken test cannot hang CI).
                    let g = lock.lock().unwrap();
                    let _ = cv
                        .wait_timeout_while(g, Duration::from_secs(20), |c| !*c)
                        .unwrap();
                    let task = serde_json::json!({
                        "id": req["params"]["task_id"],
                        "status": { "state": "canceled" },
                    });
                    let reply = serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":task});
                    let _ = writeln!(w, "{reply}");
                }
            });
        }
    });

    let (f, stop) = flag();
    let proc = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("s1".into()),
            description: "research".into(),
            delegate_to: Some("member".into()),
            ..Default::default()
        }],
    };
    let flip = {
        let f = f.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            f.store(true, Ordering::SeqCst);
        })
    };
    let started = std::time::Instant::now();
    let out = execute_dag(
        home,
        "t",
        &proc,
        &DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "r".into(),
            stop: Some(stop),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    flip.await.unwrap();
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the step waited out the member turn: {:?}",
        started.elapsed()
    );

    let seen: Vec<_> = rx.try_iter().collect();
    let delegated = seen
        .iter()
        .find(|(m, _)| m == "channel/delegate")
        .expect("delegated");
    let cancel = seen
        .iter()
        .find(|(m, _)| m == "tasks/cancel")
        .expect("tasks/cancel sent to the member");
    assert_eq!(
        cancel.1["id"], delegated.1["task_id"],
        "cancel must name the delegation's own task id"
    );
}

/// #2083 P3: a delegation whose dial fails after the member ran the turn is
/// not counted as free. The step's usage comes from the member's `tasks/get`
/// for the delegation's own task id.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_delegate_dial_still_reports_what_the_member_spent() {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping a_failed_delegate_dial_still_reports: set MUR_TEST_SOCKETS=1");
        return;
    }
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("t").unwrap();

    // Fake member: the delegate reply is lost (an RPC error stands in for a
    // timeout or a cut socket), but the turn ran and `tasks/get` says so.
    let sock = home.join("m.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let agent_dir = home.join("agents").join("member");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("running.lock"),
        serde_json::json!({
            "schema": 1, "uuid": "u", "name": "member", "pid": 1, "ppid": 1,
            "started_at": "t", "binary_version": "test", "proto_version": 1,
            "transports": { "stdio": true, "unix_socket": sock.to_str().unwrap() },
            "card_digest": "d", "capabilities": [],
        })
        .to_string(),
    )
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<(String, serde_json::Value)>();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let mut w = conn.try_clone().unwrap();
            let mut line = String::new();
            if BufReader::new(conn).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let method = req["method"].as_str().unwrap_or("").to_string();
            let _ = tx.send((method.clone(), req["params"].clone()));
            let reply = if method == "tasks/get" {
                serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":{
                    "id": req["params"]["id"], "state": "failed",
                    "usage": {
                        "input_tokens": 7_000, "output_tokens": 300,
                        "by_model": [{
                            "registry_key": "opus", "model": "claude-opus-4",
                            "input_tokens": 7_000, "output_tokens": 300,
                            "cache_read_input_tokens": 6_000,
                            "cache_creation_input_tokens": 0,
                        }],
                    },
                }})
            } else {
                serde_json::json!({"jsonrpc":"2.0","id":req["id"],
                    "error":{"code":-32000,"message":"reply lost"}})
            };
            let _ = writeln!(w, "{reply}");
        }
    });

    let seen_usage = Arc::new(std::sync::Mutex::new(None::<TokenUsage>));
    let on_step: Arc<dyn Fn(StepEvent) + Send + Sync> = {
        let seen = seen_usage.clone();
        Arc::new(move |e: StepEvent| {
            if matches!(e.kind, StepEventKind::Failed) {
                *seen.lock().unwrap() = Some(e.usage);
            }
        })
    };
    let proc = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("s1".into()),
            description: "research".into(),
            delegate_to: Some("member".into()),
            ..Default::default()
        }],
    };
    let out = execute_dag(
        home,
        "t",
        &proc,
        &DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "r".into(),
            on_step: Some(on_step),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        out.tokens_used, 7_300,
        "the failed step's spend reaches the run"
    );
    let usage = seen_usage.lock().unwrap().clone().expect("a Failed event");
    assert_eq!(usage.by_model.len(), 1);
    assert_eq!(usage.by_model[0].cache_read, 6_000);

    let seen: Vec<_> = rx.try_iter().collect();
    let delegated = seen
        .iter()
        .find(|(m, _)| m == "channel/delegate")
        .expect("delegated");
    let asked = seen
        .iter()
        .find(|(m, _)| m == "tasks/get")
        .expect("tasks/get sent after the dial failed");
    assert_eq!(
        asked.1["id"], delegated.1["task_id"],
        "usage is asked for under the delegation's own task id"
    );
}
