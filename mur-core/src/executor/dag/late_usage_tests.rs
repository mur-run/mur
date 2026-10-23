//! #2090: a delegate dial that fails while the member's turn is still running.
//!
//! Every test stands up a fake member on a unix socket, so they are gated
//! behind `MUR_TEST_SOCKETS=1` like the other socket tests.

#![cfg(unix)]

use super::*;
use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use std::time::Duration;

type Reply = Box<dyn Fn(&str, &serde_json::Value) -> serde_json::Value + Send>;

fn sockets_enabled(test: &str) -> bool {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipping {test}: set MUR_TEST_SOCKETS=1");
    false
}

/// A fake running member named `member` under `home`. `reply` answers each
/// request with the JSON-RPC `result` (or a `{"error": …}` object); every
/// request is also sent down the returned channel.
fn fake_member(home: &Path, reply: Reply) -> Receiver<(String, serde_json::Value)> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

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
    let (tx, rx) = std::sync::mpsc::channel();
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
            let out = reply(&method, &req["params"]);
            let msg = match out.get("error") {
                Some(err) => serde_json::json!({"jsonrpc":"2.0","id":req["id"],"error":err}),
                None => serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":out}),
            };
            let _ = writeln!(w, "{msg}");
        }
    });
    rx
}

fn lost_reply() -> serde_json::Value {
    serde_json::json!({"error": {"code": -32603, "message": "reply lost"}})
}

fn spent() -> serde_json::Value {
    serde_json::json!({"input_tokens": 4_000, "output_tokens": 200})
}

/// A member that is still `working` when asked, and only ends (and records
/// its usage) once it is told to cancel.
fn working_until_cancelled() -> Reply {
    let cancelled = Mutex::new(false);
    Box::new(move |method, params| match method {
        "tasks/cancel" => {
            *cancelled.lock().unwrap() = true;
            serde_json::json!({"id": params["id"], "state": "cancelled"})
        }
        "tasks/get" if *cancelled.lock().unwrap() => {
            serde_json::json!({"id": params["id"], "state": "cancelled", "usage": spent()})
        }
        "tasks/get" => serde_json::json!({"id": params["id"], "state": "working"}),
        _ => lost_reply(),
    })
}

#[tokio::test]
async fn a_turn_still_running_is_cancelled_and_its_usage_waited_for() {
    if !sockets_enabled("a_turn_still_running_is_cancelled") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let rx = fake_member(tmp.path(), working_until_cancelled());

    let got =
        late_usage::recover_delegate_usage(tmp.path(), "member", "ct-1", Duration::from_secs(10))
            .await;

    match got {
        late_usage::Recovered::Known(u) => assert_eq!(u.total(), 4_200),
        other => panic!("expected the late usage, got {other:?}"),
    }
    let seen: Vec<_> = rx.try_iter().collect();
    let cancel = seen
        .iter()
        .find(|(m, _)| m == "tasks/cancel")
        .expect("the unwanted turn is cancelled");
    assert_eq!(cancel.1["id"], "ct-1");
}

#[tokio::test]
async fn a_turn_the_member_never_started_spent_nothing() {
    if !sockets_enabled("a_turn_the_member_never_started") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let _rx = fake_member(
        tmp.path(),
        Box::new(|method, params| match method {
            "tasks/get" => serde_json::json!({"error": {
                "code": -32000,
                "message": format!("task not found: {}", params["id"].as_str().unwrap_or("")),
            }}),
            _ => lost_reply(),
        }),
    );

    let got =
        late_usage::recover_delegate_usage(tmp.path(), "member", "ct-1", Duration::from_secs(10))
            .await;

    match got {
        late_usage::Recovered::Known(u) => assert!(u.is_zero()),
        other => panic!("a turn that never ran is a known zero, got {other:?}"),
    }
}

#[tokio::test]
async fn a_turn_that_never_ends_is_unknown_after_the_wait_not_zero() {
    if !sockets_enabled("a_turn_that_never_ends") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let _rx = fake_member(
        tmp.path(),
        Box::new(|method, params| match method {
            "tasks/get" => serde_json::json!({"id": params["id"], "state": "working"}),
            "tasks/cancel" => serde_json::json!({"id": params["id"], "state": "cancelled"}),
            _ => lost_reply(),
        }),
    );

    let started = std::time::Instant::now();
    let got = late_usage::recover_delegate_usage(
        tmp.path(),
        "member",
        "ct-1",
        Duration::from_millis(1_500),
    )
    .await;

    assert!(
        matches!(got, late_usage::Recovered::Unknown(_)),
        "got {got:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the wait is bounded: {:?}",
        started.elapsed()
    );
}

/// End to end: the step that could not learn its spend says so in the
/// channel instead of reporting a silent zero.
#[tokio::test]
async fn an_unrecoverable_spend_is_warned_about_in_the_channel() {
    if !sockets_enabled("an_unrecoverable_spend_is_warned_about") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("t").unwrap();
    // An older runtime: the turn ended but `tasks/get` carries no usage.
    let _rx = fake_member(
        home,
        Box::new(|method, params| match method {
            "tasks/get" => serde_json::json!({"id": params["id"], "state": "completed"}),
            _ => lost_reply(),
        }),
    );
    let proc = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("s1".into()),
            description: "research".into(),
            delegate_to: Some("member".into()),
            ..Default::default()
        }],
    };
    execute_dag(
        home,
        "t",
        &proc,
        &DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "r".into(),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();

    let events = svc.load_events(&ch.id).unwrap();
    let warned = events.iter().any(|e| {
        e.payload
            .get("text")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.contains("spend unknown"))
    });
    assert!(warned, "no spend-unknown warning in the channel");
}

/// #2090 acceptance: a turn that outlives its failed dial is still billed to
/// the one-shot guard, and when that late spend crosses `cost_usd` the run
/// stops — the later rank never starts.
#[tokio::test]
async fn late_usage_past_the_cap_trips_the_one_shot_guard() {
    use crate::cmd::fleet::oneshot_budget::{AgentBilling, OneShotBudget};
    use mur_common::model::{BillingMode, ModelEntry};
    use mur_common::pipeline::PipelineStatus;
    use std::collections::{BTreeMap, HashMap};

    if !sockets_enabled("late_usage_past_the_cap_trips_the_one_shot_guard") {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("t").unwrap();
    let _rx = fake_member(home, working_until_cancelled());

    // `member` bills an unpriced metered model, so its 4_200 late tokens cost
    // $4.20 at the $1/1k fallback — past the $1 cap.
    let agents = HashMap::from([(
        "member".to_string(),
        AgentBilling {
            bills: true,
            model_ref: Some("metered".into()),
        },
    )]);
    let models = BTreeMap::from([(
        "metered".to_string(),
        ModelEntry {
            billing: Some(BillingMode::UsageBilled),
            ..Default::default()
        },
    )]);
    let b = Arc::new(OneShotBudget::new(
        1.0,
        1.0,
        "router".into(),
        agents,
        models,
    ));

    let marker = home.join("rank1-ran");
    let proc = Procedure {
        variables: vec![],
        steps: vec![
            ProcedureStep {
                id: Some("s0".into()),
                description: "research".into(),
                delegate_to: Some("member".into()),
                ..Default::default()
            },
            // Independent of s0, but placed in a later rank by a no-op
            // dependency, so only the guard can keep it from starting.
            ProcedureStep {
                id: Some("gate".into()),
                command: Some("true".into()),
                description: "gate".into(),
                ..Default::default()
            },
            ProcedureStep {
                id: Some("s1".into()),
                command: Some(format!("touch {}", marker.display())),
                description: "after".into(),
                depends_on: vec!["gate".into()],
                ..Default::default()
            },
        ],
    };
    let out = execute_dag(
        home,
        "t",
        &proc,
        &DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "r".into(),
            on_step: Some(b.observer()),
            stop: Some(b.stop_signal(Arc::new(|| false))),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();

    assert!(
        (b.spent_usd() - 4.2).abs() < 1e-9,
        "the late turn is billed: ${}",
        b.spent_usd()
    );
    assert!(b.tripped(), "late spend past the cap trips the guard");
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(!marker.exists(), "no step may start past the cap");
}
