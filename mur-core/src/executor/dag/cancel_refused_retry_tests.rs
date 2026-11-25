//! #2503: a refused `tasks/cancel` during an `on_failure: retry` attempt.
//!
//! Stands up a fake member on a unix socket, so it is gated behind
//! `MUR_TEST_SOCKETS=1` like the other socket tests.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The first attempt fails normally, so the step enters its retry loop. The
/// kill-switch trips during the first (non-final) retry and the member refuses
/// the cancel. The member may still be running: the run must end `Failed`, and
/// no further retry may be started.
#[cfg(unix)]
#[tokio::test]
async fn a_refused_cancel_during_a_non_final_retry_ends_failed() {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!(
            "skipping a_refused_cancel_during_a_non_final_retry_ends_failed: \
             set MUR_TEST_SOCKETS=1"
        );
        return;
    }
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let svc = ChannelService::open(home).unwrap();
    let ch = svc
        .create_for_workflow("t", mur_channel::service::ChannelOwner::Legacy)
        .unwrap();

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

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop: StopSignal = {
        let f = stop_flag.clone();
        Arc::new(move || f.load(Ordering::SeqCst))
    };
    let delegates = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));

    // Fake member. Delegate #1 fails at once (an ordinary failure → retry).
    // Delegate #2 (retry 1 of 3) trips the kill-switch itself, holds until
    // `tasks/cancel` arrives, and the cancel is REFUSED.
    {
        let (stop_flag, delegates) = (stop_flag.clone(), delegates.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { break };
                let (cancelled, stop_flag, delegates) =
                    (cancelled.clone(), stop_flag.clone(), delegates.clone());
                std::thread::spawn(move || {
                    let mut w = conn.try_clone().unwrap();
                    let mut line = String::new();
                    if BufReader::new(conn).read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let method = req["method"].as_str().unwrap_or("").to_string();
                    let (lock, cv) = &*cancelled;
                    let reply = match method.as_str() {
                        "tasks/cancel" => {
                            *lock.lock().unwrap() = true;
                            cv.notify_all();
                            serde_json::json!({"jsonrpc":"2.0","id":req["id"],
                                "error":{"code":-32000,"message":"cancel refused"}})
                        }
                        "channel/delegate" => {
                            if delegates.fetch_add(1, Ordering::SeqCst) == 0 {
                                serde_json::json!({"jsonrpc":"2.0","id":req["id"],
                                    "error":{"code":-32000,"message":"first attempt failed"}})
                            } else {
                                stop_flag.store(true, Ordering::SeqCst);
                                let g = lock.lock().unwrap();
                                let _ = cv
                                    .wait_timeout_while(g, Duration::from_secs(20), |c| !*c)
                                    .unwrap();
                                serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":{
                                    "id": req["params"]["task_id"],
                                    "status": { "state": "canceled" },
                                }})
                            }
                        }
                        _ => serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":{}}),
                    };
                    let _ = writeln!(w, "{reply}");
                });
            }
        });
    }

    let proc = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("s1".into()),
            description: "research".into(),
            delegate_to: Some("member".into()),
            on_failure: FailureAction::Retry,
            retry: Some(mur_common::skill::manifest::RetryConfig {
                max_retries: 3,
                backoff_secs: None,
            }),
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
            stop: Some(stop),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(
        out.status,
        PipelineStatus::Failed,
        "a cancel refused during a retry leaves the member running: Failed, not Stopped"
    );
    assert_eq!(
        delegates.load(Ordering::SeqCst),
        2,
        "no further attempt may be delegated after a refused cancel"
    );
}
