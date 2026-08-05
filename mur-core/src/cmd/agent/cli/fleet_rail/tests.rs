use super::*;
use serde_json::json;

fn ev(seq: u64, actor: ChannelActor, kind: EventKind, payload: serde_json::Value) -> ChannelEvent {
    ChannelEvent {
        seq,
        ts: DateTime::parse_from_rfc3339("2026-07-29T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        actor,
        kind,
        payload,
        idempotency_key: None,
        sig: None,
        key_version: None,
    }
}

fn agent(id: &str) -> ChannelActor {
    ChannelActor::Agent { id: id.into() }
}

#[test]
fn state_changes_map_to_member_states() {
    let evs = vec![
        ev(
            1,
            agent("qa"),
            EventKind::StateChange,
            json!({"from": "submitted", "to": "working"}),
        ),
        ev(
            2,
            agent("backend"),
            EventKind::StateChange,
            json!({"from": "working", "to": "completed"}),
        ),
        ev(
            3,
            agent("dataml"),
            EventKind::StateChange,
            json!({"from": "working", "to": "failed"}),
        ),
        ev(
            4,
            agent("pm"),
            EventKind::StateChange,
            json!({"from": "working", "to": "canceled"}),
        ),
    ];
    let rows = fold_members(&evs);
    let by = |n: &str| rows.iter().find(|r| r.agent == n).unwrap().state.clone();
    assert!(matches!(by("qa"), MemberState::Working { .. }));
    assert!(matches!(by("backend"), MemberState::Done));
    // canceled and rejected collapse into failed — the user only needs
    // "it did not finish".
    assert!(matches!(by("dataml"), MemberState::Failed));
    assert!(matches!(by("pm"), MemberState::Failed));
}

#[test]
fn a_hitl_request_blocks_and_its_response_unblocks() {
    let req = json!({"hitl_id": "h1", "tool_name": "bash", "summary": "cargo publish", "action_hash": "x", "tier": "write"});
    let evs = vec![
        ev(
            1,
            agent("qa"),
            EventKind::StateChange,
            json!({"to": "working"}),
        ),
        ev(2, agent("qa"), EventKind::HitlRequest, req),
    ];
    let rows = fold_members(&evs);
    match &rows[0].state {
        MemberState::Blocked { summary, hitl_id } => {
            assert_eq!(hitl_id, "h1");
            assert!(summary.contains("cargo publish"));
        }
        other => panic!("expected blocked, got {other:?}"),
    }

    // The approval is written by the HUMAN, not by the blocked agent, so
    // clearing must key on hitl_id — never on the actor.
    let mut evs = evs;
    evs.push(ev(
        3,
        ChannelActor::Human {
            name: "david".into(),
        },
        EventKind::HitlResponse,
        json!({"hitl_id": "h1", "allow": true, "surface": "cli"}),
    ));
    let rows = fold_members(&evs);
    assert!(matches!(rows[0].state, MemberState::Working { .. }));
}

#[test]
fn tool_calls_annotate_the_working_row() {
    let evs = vec![
        ev(
            1,
            agent("qa"),
            EventKind::StateChange,
            json!({"to": "working"}),
        ),
        ev(
            2,
            agent("qa"),
            EventKind::ToolCall,
            json!({"tool": "bash", "command": "cargo test"}),
        ),
    ];
    let rows = fold_members(&evs);
    match &rows[0].state {
        MemberState::Working { tool, .. } => assert_eq!(tool.as_deref(), Some("cargo test")),
        other => panic!("expected working, got {other:?}"),
    }
}

#[test]
fn human_and_system_actors_never_become_rows() {
    let evs = vec![
        ev(
            1,
            ChannelActor::Human {
                name: "david".into(),
            },
            EventKind::Message,
            json!({"text": "go"}),
        ),
        ev(
            2,
            ChannelActor::System,
            EventKind::StateChange,
            json!({"to": "working"}),
        ),
    ];
    assert!(fold_members(&evs).is_empty());
}

#[test]
fn blocked_sorts_first_then_working_then_finished() {
    let evs = vec![
        ev(
            1,
            agent("aaa_done"),
            EventKind::StateChange,
            json!({"to": "completed"}),
        ),
        ev(
            2,
            agent("bbb_working"),
            EventKind::StateChange,
            json!({"to": "working"}),
        ),
        ev(
            3,
            agent("ccc_blocked"),
            EventKind::HitlRequest,
            json!({"hitl_id": "h1", "tool_name": "bash", "summary": "rm", "action_hash": "x", "tier": "write"}),
        ),
    ];
    let rows = fold_members(&evs);
    let names: Vec<&str> = rows.iter().map(|r| r.agent.as_str()).collect();
    assert_eq!(names, vec!["ccc_blocked", "bbb_working", "aaa_done"]);
}

#[test]
fn an_empty_channel_has_no_rows() {
    assert!(fold_members(&[]).is_empty());
}

#[test]
fn state_change_input_required_blocks_with_empty_hitl_id() {
    let evs = vec![ev(
        1,
        agent("qa"),
        EventKind::StateChange,
        json!({"from": "working", "to": "input-required"}),
    )];
    let rows = fold_members(&evs);
    match &rows[0].state {
        MemberState::Blocked { hitl_id, .. } => assert_eq!(hitl_id, ""),
        other => panic!("expected blocked, got {other:?}"),
    }

    // Pin the quirk: a StateChange-driven block carries no hitl_id, so a
    // HitlResponse (which only matches by hitl_id) can never clear it —
    // only a later StateChange can. This is intentional, not a bug.
    let mut with_response = evs.clone();
    with_response.push(ev(
        2,
        ChannelActor::Human {
            name: "david".into(),
        },
        EventKind::HitlResponse,
        json!({"hitl_id": "some-other-id", "allow": true, "surface": "cli"}),
    ));
    assert!(
        matches!(
            fold_members(&with_response)[0].state,
            MemberState::Blocked { .. }
        ),
        "a HitlResponse must not clear a StateChange-driven block"
    );

    let mut with_state_change = evs;
    with_state_change.push(ev(
        2,
        agent("qa"),
        EventKind::StateChange,
        json!({"from": "input-required", "to": "working"}),
    ));
    assert!(
        matches!(
            fold_members(&with_state_change)[0].state,
            MemberState::Working { .. }
        ),
        "a later StateChange DOES clear a StateChange-driven block"
    );
}

#[test]
fn tool_call_does_not_resurrect_a_finished_or_blocked_member() {
    for to in ["completed", "failed"] {
        let evs = vec![
            ev(1, agent("qa"), EventKind::StateChange, json!({"to": to})),
            ev(
                2,
                agent("qa"),
                EventKind::ToolCall,
                json!({"tool": "bash", "command": "rm -rf /"}),
            ),
        ];
        let rows = fold_members(&evs);
        let expected = if to == "completed" {
            MemberState::Done
        } else {
            MemberState::Failed
        };
        assert_eq!(
            rows[0].state, expected,
            "a ToolCall must not change a finished member's state ({to})"
        );
    }

    let evs = vec![
        ev(
            1,
            agent("qa"),
            EventKind::HitlRequest,
            json!({"hitl_id": "h1", "tool_name": "bash", "summary": "cargo publish", "action_hash": "x", "tier": "write"}),
        ),
        ev(
            2,
            agent("qa"),
            EventKind::ToolCall,
            json!({"tool": "bash", "command": "echo hi"}),
        ),
    ];
    let rows = fold_members(&evs);
    assert!(
        matches!(rows[0].state, MemberState::Blocked { .. }),
        "a ToolCall must not unblock a blocked member"
    );
}

#[test]
fn approving_one_hitl_id_does_not_unblock_a_different_one() {
    let evs = vec![
        ev(
            1,
            agent("qa"),
            EventKind::HitlRequest,
            json!({"hitl_id": "h1", "tool_name": "bash", "summary": "rm", "action_hash": "x", "tier": "write"}),
        ),
        ev(
            2,
            agent("backend"),
            EventKind::HitlRequest,
            json!({"hitl_id": "h2", "tool_name": "bash", "summary": "deploy", "action_hash": "y", "tier": "write"}),
        ),
        ev(
            3,
            ChannelActor::Human {
                name: "david".into(),
            },
            EventKind::HitlResponse,
            json!({"hitl_id": "h1", "allow": true, "surface": "cli"}),
        ),
    ];
    let rows = fold_members(&evs);
    let by = |n: &str| rows.iter().find(|r| r.agent == n).unwrap().state.clone();
    assert!(
        matches!(by("qa"), MemberState::Working { .. }),
        "h1 approved: qa must unblock"
    );
    match by("backend") {
        MemberState::Blocked { hitl_id, .. } => {
            assert_eq!(
                hitl_id, "h2",
                "backend must stay blocked on its own hitl_id"
            )
        }
        other => panic!("expected backend still blocked, got {other:?}"),
    }
}

#[test]
fn state_change_submitted_maps_to_working() {
    let evs = vec![ev(
        1,
        agent("qa"),
        EventKind::StateChange,
        json!({"to": "submitted"}),
    )];
    let rows = fold_members(&evs);
    assert!(matches!(rows[0].state, MemberState::Working { .. }));
}

use mur_common::fleet::{Job, JobStatus};

fn job(id: &str, status: JobStatus) -> Job {
    Job {
        id: id.into(),
        text: "do the thing".into(),
        source: "cli".into(),
        status,
        created_at: "2026-07-29T00:00:00Z".into(),
        started_at: None,
        finished_at: None,
        run_id: None,
        result: None,
        error: None,
    }
}

#[test]
fn jobs_line_counts_terminal_over_total() {
    let jobs = vec![
        job("1", JobStatus::Done),
        job("2", JobStatus::Failed),
        job("3", JobStatus::Running),
        job("4", JobStatus::Queued),
        job("5", JobStatus::Queued),
    ];
    // 2 of 5 have reached a terminal state; one of those failed.
    let line = jobs_line("develop", &jobs, false);
    assert!(line.contains("fleet · develop"), "got: {line}");
    assert!(line.contains("job 2/5"), "got: {line}");
    assert!(line.contains("1 ⏵ running"), "got: {line}");
    assert!(line.contains("1 ✖ failed"), "got: {line}");
}

#[test]
fn jobs_line_says_not_run_yet_when_there_are_none() {
    let line = jobs_line("develop", &[], false);
    assert!(line.contains("not run yet"), "got: {line}");
    assert!(line.contains("mur fleet run develop"), "got: {line}");
}

#[test]
fn jobs_line_omits_the_failed_clause_when_nothing_failed() {
    let line = jobs_line("develop", &[job("1", JobStatus::Done)], false);
    assert!(!line.contains("failed"), "got: {line}");
}

use std::time::Instant;

/// A fleet channel with one member. `create_for_fleet(fleet_name, router,
/// members)` names the channel `fleet-<fleet_name>` itself — the rail must
/// derive the same id from `--fleet dev`.
fn seed_home() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    svc.create_for_fleet("dev", "mur", &["qa".to_string()])
        .unwrap();
    tmp
}

#[test]
fn poll_reports_change_only_when_the_log_grows() {
    let tmp = seed_home();
    let now = Instant::now();
    let mut rail = FleetRail::start("dev");

    // First poll reads the (empty) channel and the (absent) job dir.
    assert!(rail.poll(tmp.path(), now), "first poll must produce a view");
    assert!(rail.view().members.is_empty());
    assert!(rail.view().jobs_line.contains("not run yet"));

    // Nothing changed → no work, no change reported.
    assert!(!rail.poll(tmp.path(), now));

    // A member acts → the next poll picks it up.
    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    svc.append(
        "fleet-dev",
        ChannelActor::Agent { id: "qa".into() },
        EventKind::StateChange,
        serde_json::json!({"to": "working"}),
        None,
    )
    .unwrap();
    assert!(rail.poll(tmp.path(), now), "log grew → view must change");
    assert_eq!(rail.view().members.len(), 1);
    assert_eq!(rail.view().members[0].agent, "qa");
}

#[test]
fn an_unreadable_channel_degrades_instead_of_failing() {
    let tmp = tempfile::TempDir::new().unwrap(); // no channel at all
    let mut rail = FleetRail::start("ghost");
    rail.poll(tmp.path(), Instant::now());
    assert!(rail.view().members.is_empty());
    // The rail says so on its own line; it never returns Err.
    assert!(rail.view().notice.is_some());
}

#[test]
fn poll_reconciles_a_running_job_against_channel_truth() {
    let tmp = seed_home();
    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    // The run drove the channel to a terminal state...
    svc.append(
        "fleet-dev",
        ChannelActor::System,
        EventKind::StateChange,
        serde_json::json!({"from": "working", "to": "completed"}),
        None,
    )
    .unwrap();
    // ...but crashed before stamping the job yaml, so the store still
    // says `running` (the exact gap `reconcile_running` exists to close).
    let mut j = job("1", JobStatus::Running);
    j.run_id = Some("run-1".to_string());
    crate::cmd::fleet::jobs::save_job(tmp.path(), "dev", &j).unwrap();

    let mut rail = FleetRail::start("dev");
    rail.poll(tmp.path(), Instant::now());
    let line = &rail.view().jobs_line;
    assert!(
        line.contains("job 1/1"),
        "channel-terminal job must count toward the terminal total, got: {line}"
    );
    assert!(
        !line.contains("running"),
        "must not still report it running, got: {line}"
    );
}

#[test]
fn poll_falls_back_to_channel_summary_when_jobs_store_is_unreadable() {
    let tmp = seed_home();
    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    svc.append(
        "fleet-dev",
        ChannelActor::Agent { id: "qa".into() },
        EventKind::StateChange,
        serde_json::json!({"to": "working"}),
        None,
    )
    .unwrap();

    let dir = crate::cmd::fleet::jobs::jobs_dir(tmp.path(), "dev");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.yaml"), "not: valid: yaml:").unwrap();

    let mut rail = FleetRail::start("dev");
    rail.poll(tmp.path(), Instant::now());
    let view = rail.view();
    assert!(
        !view.jobs_line.contains("not run yet"),
        "an unreadable job store is not \"never run\" — got: {}",
        view.jobs_line
    );
    assert!(
        view.notice
            .as_deref()
            .unwrap_or("")
            .contains("jobs unreadable"),
        "notice must surface the unreadable store, got: {:?}",
        view.notice
    );
}

// ── Production delegate-path shapes (v3d-2) ─────────────────────────────────
// Real fleet channels contain System delegations (turn start) and the
// member's own signed reply Message (turn end) — never member-emitted
// state-changes. The fold must produce rows from THOSE.

#[test]
fn system_delegation_starts_a_member_row_with_its_goal() {
    let evs = vec![
        ev(
            1,
            ChannelActor::System,
            EventKind::Delegation,
            json!({
                "target_agent": "dr_worker_1",
                "child_task_id": "ct-1",
                "parent_channel_id": "fleet-deep-research",
                "goal": "survey memory designs"
            }),
        ),
        ev(
            2,
            ChannelActor::System,
            EventKind::StateChange,
            json!({"from": "submitted", "to": "working"}),
        ),
    ];
    let rows = fold_members(&evs);
    assert_eq!(rows.len(), 1, "System state-changes must not become rows");
    assert_eq!(rows[0].agent, "dr_worker_1");
    match &rows[0].state {
        MemberState::Working { tool, .. } => {
            assert_eq!(tool.as_deref(), Some("survey memory designs"));
        }
        s => panic!("expected Working, got {s:?}"),
    }
}

#[test]
fn member_reply_marks_done_and_redelegation_restarts() {
    let deleg = |seq| {
        ev(
            seq,
            ChannelActor::System,
            EventKind::Delegation,
            json!({"target_agent": "qa", "child_task_id": "ct", "parent_channel_id": "fleet-x"}),
        )
    };
    let reply = |seq| {
        ev(
            seq,
            agent("qa"),
            EventKind::Message,
            json!({"text": "## Summary\nall done"}),
        )
    };
    let rows = fold_members(&[deleg(1), reply(2)]);
    assert!(
        matches!(rows[0].state, MemberState::Done),
        "reply ends the turn: {:?}",
        rows[0].state
    );
    let rows = fold_members(&[deleg(1), reply(2), deleg(3)]);
    assert!(
        matches!(rows[0].state, MemberState::Working { .. }),
        "re-delegation restarts the member: {:?}",
        rows[0].state
    );
}

#[test]
fn member_message_never_clears_a_hitl_block() {
    let evs = vec![
        ev(
            1,
            agent("qa"),
            EventKind::HitlRequest,
            json!({"summary": "git push", "hitl_id": "h-1"}),
        ),
        ev(
            2,
            agent("qa"),
            EventKind::Message,
            json!({"text": "waiting"}),
        ),
    ];
    let rows = fold_members(&evs);
    assert!(
        matches!(rows[0].state, MemberState::Blocked { .. }),
        "got {:?}",
        rows[0].state
    );
}

#[test]
fn jobs_line_flags_an_in_flight_goal_run() {
    // Goal-mode runs never touch the job store: an empty store must read as
    // in-progress, not "not run yet"…
    let line = jobs_line("develop", &[], true);
    assert!(line.contains("run in progress"), "got: {line}");
    assert!(!line.contains("not run yet"), "got: {line}");
    // …and stale terminal jobs must not read as "all finished".
    let line = jobs_line("develop", &[job("1", JobStatus::Done)], true);
    assert!(line.contains("run in progress"), "got: {line}");
}
