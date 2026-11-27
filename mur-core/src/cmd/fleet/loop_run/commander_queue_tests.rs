use super::outcome_tests::run_loop_for_test;
use super::*;

#[tokio::test]
async fn commander_kill_halts_loop_and_local_start_cannot_clear_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // commander identity + pinned key
    let cdir = home.join("commander");
    std::fs::create_dir_all(&cdir).unwrap();
    mur_common::identity::AgentIdentity::generate()
        .save(&cdir)
        .unwrap();
    // a fleet + channel
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "g".into(),
        router: None,
        members: vec!["pm".into()],
        team_id: None,
        channel_id: "fleet-dev".into(),
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
    crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
    mur_channel::ChannelService::open(home)
        .unwrap()
        .create_for_fleet(
            "dev",
            "mur",
            &["pm".into()],
            mur_channel::service::ChannelOwner::Legacy,
        )
        .unwrap();
    // plant a commander kill
    crate::cmd::commander::cmd_commander_directive(home, "dev", "kill", None, 1000).unwrap();

    // one guarded run: must stop CommanderKilled before doing any work
    let stop = run_loop_for_test(home).await;
    assert_eq!(stop, LoopStop::CommanderKilled);

    // local kill-switch clear does NOT lift the commander kill
    crate::cmd::fleet::control::cmd_fleet_start(home, "dev").ok();
    let stop2 = run_loop_for_test(home).await;
    assert_eq!(stop2, LoopStop::CommanderKilled);

    // an audit Governance entry was recorded
    let audit = std::fs::read_to_string(home.join("conversations").join("audit.jsonl")).unwrap();
    assert!(audit.contains("\"kind\":\"governance\"") && audit.contains("\"decision\":\"halted\""));
}

#[tokio::test]
async fn commander_zero_budget_ceiling_halts_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let cdir = home.join("commander");
    std::fs::create_dir_all(&cdir).unwrap();
    mur_common::identity::AgentIdentity::generate()
        .save(&cdir)
        .unwrap();
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "g".into(),
        router: None,
        members: vec!["pm".into()],
        team_id: None,
        channel_id: "fleet-dev".into(),
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
    crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
    mur_channel::ChannelService::open(home)
        .unwrap()
        .create_for_fleet(
            "dev",
            "mur",
            &["pm".into()],
            mur_channel::service::ChannelOwner::Legacy,
        )
        .unwrap();
    // a zero budget ceiling is a budget halt (spec §6), not a kill
    crate::cmd::commander::cmd_commander_directive(home, "dev", "budget_ceiling", Some(0.0), 1000)
        .unwrap();

    let stop = run_loop_for_test(home).await;
    assert_eq!(stop, LoopStop::Budget);

    // The audit row must bind the EXACT deciding directive — pull its real
    // nonce from the channel and assert the audit references it (guards the
    // wire-through: the loop passes gov.budget_nonce, not a constant).
    let nonce = mur_channel::ChannelService::open(home)
        .unwrap()
        .load_events("fleet-dev")
        .unwrap()
        .iter()
        .find_map(|e| {
            e.payload
                .get("commander_directive")
                .and_then(|d| d.get("nonce"))
                .and_then(|n| n.as_str())
                .map(str::to_string)
        })
        .expect("directive nonce present in channel");
    let audit = std::fs::read_to_string(home.join("conversations").join("audit.jsonl")).unwrap();
    assert!(
        audit.contains("\"decision\":\"capped\"")
            && audit.contains("\"directive\":\"budget_ceiling\"")
    );
    assert!(
        audit.contains(&format!("\"nonce\":\"{nonce}\"")),
        "audit must bind the exact deciding directive nonce ({nonce})"
    );
}

#[test]
fn queue_drained_outcome_has_its_own_label() {
    // The progress file's `outcome` is how a caller tells "finished because
    // there was nothing left to do" from "ran out of iterations".
    assert_eq!(outcome_label(LoopStop::QueueDrained), "queue-drained");
    assert_ne!(
        outcome_label(LoopStop::QueueDrained),
        outcome_label(LoopStop::MaxIterations)
    );
}

#[tokio::test]
async fn queue_drained_break_fires_when_policy_is_queue_empty_and_queue_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "standing goal".into(),
        router: None,
        members: vec!["pm".into()],
        team_id: None,
        channel_id: "fleet-dev".into(),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: Some(mur_common::fleet::FleetLoop {
            trigger: "manual".into(),
            max_iterations: 1,
            budget_usd: 0.0,
            deadline: String::new(),
            done_when: super::super::done_policy::DONE_WHEN_QUEUE_EMPTY.into(),
        }),
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    };
    crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
    mur_channel::ChannelService::open(home)
        .unwrap()
        .create_for_fleet(
            "dev",
            "mur",
            &["pm".into()],
            mur_channel::service::ChannelOwner::Legacy,
        )
        .unwrap();
    // No job is ever queued, so the break fires on iteration 1 — ahead of
    // the `plan_via_router` dial, so no live "pm" agent is needed here.
    let stop = run_loop_for_test(home).await;
    assert_eq!(stop, LoopStop::QueueDrained);
}

#[tokio::test]
async fn queue_drained_break_stays_inert_under_router_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "standing goal".into(),
        router: None,
        members: vec!["pm".into()],
        team_id: None,
        channel_id: "fleet-dev".into(),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: Some(mur_common::fleet::FleetLoop {
            trigger: "manual".into(),
            max_iterations: 1,
            budget_usd: 0.0,
            deadline: String::new(),
            done_when: String::new(), // router policy — the fallback for empty/legacy values
        }),
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        // The iteration errors (no live "pm") and produces no agent event,
        // so a zero stuck window ends the loop right after it — the old
        // `max_iterations: 1` no longer caps anything (spec §6), and
        // without this the test would sit out the 10-minute default.
        limits: Some(mur_common::limits::Limits {
            deadline: None,
            stuck: Some("0s".into()),
            cost_usd: None,
        }),
        needs: vec![],
    };
    crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
    mur_channel::ChannelService::open(home)
        .unwrap()
        .create_for_fleet(
            "dev",
            "mur",
            &["pm".into()],
            mur_channel::service::ChannelOwner::Legacy,
        )
        .unwrap();
    // Same empty queue as the fires case, but router policy: the gate must
    // not mistake an empty queue for `done_when: queue-empty`. No live
    // "pm" agent exists to dial, so the iteration errors out and the zero
    // stuck window stops the loop — the only claim under test is that the
    // gate did NOT mistake this for a drained queue.
    let stop = run_loop_for_test(home).await;
    assert_ne!(stop, LoopStop::QueueDrained);
}

#[tokio::test]
async fn queue_empty_policy_with_claimed_job_does_not_converge_via_router() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "standing goal".into(),
        router: None,
        members: vec!["pm".into()],
        team_id: None,
        channel_id: "fleet-dev".into(),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: Some(mur_common::fleet::FleetLoop {
            trigger: "manual".into(),
            max_iterations: 1,
            budget_usd: 0.0,
            deadline: String::new(),
            done_when: super::super::done_policy::DONE_WHEN_QUEUE_EMPTY.into(),
        }),
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    };
    crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
    mur_channel::ChannelService::open(home)
        .unwrap()
        .create_for_fleet(
            "dev",
            "mur",
            &["pm".into()],
            mur_channel::service::ChannelOwner::Legacy,
        )
        .unwrap();
    // A claimed job means `active_job` is `Some`, so the drained-queue
    // break above does NOT fire and this iteration takes the normal
    // delegate path — regression coverage for the bug where `queue-empty`
    // fell through `done_marker` (which only recognises `marker:`) into
    // `ask_router_done`, paying for an LLM call on every iteration that
    // actually does work, on a policy that promises never to make one.
    super::super::jobs::enqueue_job(home, "dev", "job-1", "cli").unwrap();
    let stop = run_loop_for_test(home).await;
    assert_ne!(stop, LoopStop::Converged);
}
