//! #2036 at the loop level: a stop requested during the iteration that would
//! have converged is recorded `stopped`, never `converged` / `done`.
//!
//! Needs a fake member on a unix socket, so it is gated behind
//! `MUR_TEST_SOCKETS=1` like the other socket tests.

use super::*;
use mur_common::fleet::{Fleet, FleetLoop, FleetStep};

#[cfg(unix)]
#[tokio::test]
async fn a_stop_during_the_converging_iteration_is_recorded_stopped() {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping a_stop_during_the_converging_iteration: set MUR_TEST_SOCKETS=1");
        return;
    }
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    const MARKER: &str = "RESEARCH_DONE";
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "research question".into(),
        router: None,
        members: vec!["member".into()],
        team_id: None,
        channel_id: "fleet-dev".into(),
        procedure: vec![FleetStep {
            id: "s1".into(),
            member: "member".into(),
            task: String::new(),
            depends_on: vec![],
        }],
        rules: vec![],
        skills: vec![],
        loop_cfg: Some(FleetLoop {
            done_when: format!("marker:{MARKER}"),
            ..serde_yaml::from_str("{}").unwrap()
        }),
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    };
    crate::cmd::fleet::store::save_fleet(&home, &fleet).unwrap();
    mur_channel::ChannelService::open(&home)
        .unwrap()
        .create_for_fleet("dev", "mur", &["member".into()])
        .unwrap();

    // Fake member: does the work (writes the convergence marker into the
    // channel), then — while its turn is still "in flight" from the loop's
    // point of view — the operator engages the kill-switch, then it replies.
    // Before #2036 this iteration converged and the run was recorded done.
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
    let member_home = home.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let mut w = conn.try_clone().unwrap();
            let mut line = String::new();
            if BufReader::new(conn).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            if req["method"] == "channel/delegate" {
                mur_channel::ChannelService::open(&member_home)
                    .unwrap()
                    .append(
                        "fleet-dev",
                        ChannelActor::Agent {
                            id: "member".into(),
                        },
                        mur_common::channel::EventKind::Message,
                        serde_json::json!({ "text": format!("findings\n{MARKER}") }),
                        None,
                    )
                    .unwrap();
                crate::cmd::fleet::control::cmd_fleet_stop(&member_home, "dev").unwrap();
            }
            let task = serde_json::json!({
                "id": req["params"]["task_id"],
                "status": { "state": "completed" },
                "messages": [{ "role": "agent", "parts": [{ "kind": "text", "text": format!("findings\n{MARKER}") }] }],
            });
            let reply = serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":task});
            let _ = writeln!(w, "{reply}");
        }
    });

    let (stop, _, _) = run_guarded(&home, "dev", Some(3), None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        stop,
        LoopStop::Stopped,
        "a stop during the run won over convergence"
    );
    let (p, _) = crate::cmd::fleet::progress::load(&home, "dev").expect("progress written");
    assert_eq!(p.outcome.as_deref(), Some("stopped"));
    assert_eq!(loop_terminal_state(stop), crate::run_status::State::Stopped);
}

/// #2116: a custom-router fleet's stop event must be signed and verify.
///
/// `emit_stop_event` writes a `System` actor, and `channel_verify` checks
/// every non-Agent actor against `ROUTER_AGENT`. Signing it with the fleet's
/// router (`router: orchestrator`) produced an event its own channel could not
/// verify even unsealed — and, in a sealed child dispatched by `mur`, an
/// UNSIGNED one: the handoff key is `mur`'s, so `orchestrator`'s key is read
/// from disk, and the seal denies `keys/`.
#[test]
fn a_custom_router_stop_event_is_signed_by_the_writer_and_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    crate::channel_writer::plant_identity_for(home, "orchestrator");
    let fleet = Fleet {
        name: "council".into(),
        display_name: String::new(),
        goal: "g".into(),
        router: Some("orchestrator".into()),
        members: vec!["orchestrator".into()],
        team_id: None,
        channel_id: "fleet-council".into(),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    };
    let svc = mur_channel::ChannelService::open(home).unwrap();
    svc.create_for_fleet("council", "orchestrator", &["orchestrator".into()])
        .unwrap();

    pause::emit_stop_event(&svc, home, &fleet, LoopStop::Budget, 0, 0.0, "run-1");

    let ev = svc.load_events("fleet-council").unwrap().pop().unwrap();
    assert_eq!(ev.kind, mur_common::channel::EventKind::StateChange);
    assert!(ev.sig.is_some(), "the stop event must be signed");
    assert!(
        crate::channel_verify::verify_event(home, "fleet-council", &ev, true),
        "a System stop event must verify against ROUTER_AGENT's key"
    );
}
