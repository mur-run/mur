//! Pause/resume design §5.2 at the loop level: a pause requested while a
//! member's turn is in flight ends `LoopStop::Paused`, leaves the claimed job
//! `Running`, charges the spend, does not count the iteration, and writes the
//! checkpoint before returning.
//!
//! Needs a fake member on a unix socket, so it is gated behind
//! `MUR_TEST_SOCKETS=1` like the other socket tests.

use super::*;
use mur_common::fleet::{Fleet, FleetLoop, FleetStep};

#[cfg(unix)]
#[tokio::test]
async fn a_pause_during_an_iteration_checkpoints_and_leaves_the_job_running() {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping a_pause_during_an_iteration: set MUR_TEST_SOCKETS=1");
        return;
    }
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    const MARKER: &str = "RESEARCH_DONE";
    const PAUSE_RUN: &str = "pause-run";
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

    let job = crate::cmd::fleet::jobs::enqueue_job(&home, "dev", "the job", "cli").unwrap();

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
                // The operator pauses while the member's turn is in flight.
                std::fs::write(
                    crate::cmd::fleet::control::paused_path(&member_home, "dev"),
                    format!("run_id: {PAUSE_RUN}\nrequested_at: t\n"),
                )
                .unwrap();
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

    let (stop, iterations, spent) = run_guarded(
        &home,
        "dev",
        Some(3),
        None,
        None,
        Some(PAUSE_RUN.into()),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(stop, LoopStop::Paused, "a pause during the run ends Paused");
    assert_eq!(iterations, 0, "the unfinished iteration is not counted");
    assert!(spent >= 0.0);

    // The claimed job is still Running, under the per-iteration run id.
    let jobs = crate::cmd::fleet::jobs::list_jobs_raw(&home, "dev").unwrap();
    let held = jobs.iter().find(|j| j.id == job.id).expect("job exists");
    assert_eq!(held.status, mur_common::fleet::JobStatus::Running);
    assert_eq!(held.run_id.as_deref(), Some("pause-run-0"));

    // The checkpoint exists, names the job and the stored procedure.
    let cp = checkpoint::load(&home, PAUSE_RUN)
        .unwrap()
        .expect("checkpoint written before the loop returned");
    assert_eq!(cp.iteration, 0);
    assert_eq!(cp.iteration_run_id, "pause-run-0");
    assert_eq!(cp.job_id.as_deref(), Some(job.id.as_str()));
    assert!(
        !cp.procedure.is_empty(),
        "the planned procedure is stored whole"
    );

    // The run is Paused, not Stopped / Done; the progress record is unfinished.
    let rec = crate::run_status::store::load(&home, PAUSE_RUN)
        .unwrap()
        .expect("run recorded");
    assert_eq!(rec.state, crate::run_status::State::Paused);
    let (p, _) = crate::cmd::fleet::progress::load(&home, "dev").expect("progress written");
    assert!(p.finished_at.is_none(), "a paused run is not finished");
    assert!(p.paused_at.is_some());
    assert_eq!(p.outcome.as_deref(), Some("paused"));

    // No terminal StateChange: the first reconcile must not cancel the job.
    let events = mur_channel::ChannelService::open(&home)
        .unwrap()
        .load_events("fleet-dev")
        .unwrap();
    assert_eq!(
        crate::cmd::fleet::jobs::channel_terminal_status(&events),
        None,
        "a pause writes no terminal channel state"
    );
    let paused_notes = events
        .iter()
        .filter(|e| e.kind == mur_common::channel::EventKind::Note)
        .filter(|e| {
            matches!(
                pause_events::classify_note_payload(&e.payload),
                pause_events::NoteClassification::FleetRun(_)
            )
        })
        .count();
    assert_eq!(paused_notes, 1, "exactly one run_paused note");
}

/// Design §10 "Resume": the run paused above is continued under its original
/// run id, re-enters the same iteration with the stored procedure, keeps the
/// claimed job, converges, and records `run_resumed`.
#[cfg(unix)]
#[tokio::test]
async fn a_paused_run_resumes_under_its_run_id_and_finishes() {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping a_paused_run_resumes: set MUR_TEST_SOCKETS=1");
        return;
    }
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MARKER: &str = "RESEARCH_DONE";
    const RUN: &str = "resume-run";
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
    let job = crate::cmd::fleet::jobs::enqueue_job(&home, "dev", "the job", "cli").unwrap();
    let other = crate::cmd::fleet::jobs::enqueue_job(&home, "dev", "later job", "cli").unwrap();

    // Fake member: the first turn is paused in flight; the second (the
    // resumed run) completes and writes the marker.
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
    let turns = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = turns.clone();
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
                let n = seen.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    std::fs::write(
                        crate::cmd::fleet::control::paused_path(&member_home, "dev"),
                        format!("run_id: {RUN}\nrequested_at: t\n"),
                    )
                    .unwrap();
                } else {
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
                }
            }
            let task = serde_json::json!({
                "id": req["params"]["task_id"],
                "status": { "state": "completed" },
                "messages": [{ "role": "agent", "parts": [{ "kind": "text", "text": "findings" }] }],
            });
            let reply = serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":task});
            let _ = writeln!(w, "{reply}");
        }
    });

    let (stop, _, _) = run_guarded(&home, "dev", None, None, None, Some(RUN.into()), None, None)
        .await
        .unwrap();
    assert_eq!(stop, LoopStop::Paused);

    // A fresh run over the paused one is refused.
    let err = run_guarded(&home, "dev", None, None, None, None, None, None)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("has a paused run"), "{err}");

    let cp = resume::admit(&home, "dev").expect("resumable");
    let from = resume::resume_from(&cp, None, true).unwrap();
    let (stop, iterations, _) = run_guarded_from(&home, "dev", None, None, Some(from))
        .await
        .unwrap();
    assert_eq!(stop, LoopStop::Converged, "the resumed run finishes");
    assert_eq!(iterations, 1, "the paused iteration is counted once");
    assert_eq!(turns.load(Ordering::SeqCst), 2, "one dispatch per run");

    // Same run, now done; nothing left to resume.
    let rec = crate::run_status::store::load(&home, RUN).unwrap().unwrap();
    assert_eq!(rec.state, crate::run_status::State::Done);
    assert!(checkpoint::load(&home, RUN).unwrap().is_none());
    assert!(!crate::cmd::fleet::control::paused_path(&home, "dev").exists());

    // The claimed job finished; the later one was never claimed by the
    // re-entered iteration.
    let jobs = crate::cmd::fleet::jobs::list_jobs_raw(&home, "dev").unwrap();
    let j = jobs.iter().find(|j| j.id == job.id).unwrap();
    assert_eq!(j.status, mur_common::fleet::JobStatus::Done);
    assert_eq!(j.run_id.as_deref(), Some("resume-run-0"));
    let o = jobs.iter().find(|j| j.id == other.id).unwrap();
    assert_eq!(o.status, mur_common::fleet::JobStatus::Queued);

    let events = mur_channel::ChannelService::open(&home)
        .unwrap()
        .load_events("fleet-dev")
        .unwrap();
    let resumed = events
        .iter()
        .filter(|e| {
            matches!(
                pause_events::classify_note_payload(&e.payload),
                pause_events::NoteClassification::FleetRun(pause_events::FleetRunEnvelope {
                    payload: pause_events::FleetRunPayload::RunResumed { .. },
                    ..
                })
            )
        })
        .count();
    assert_eq!(resumed, 1, "exactly one run_resumed note");
}
