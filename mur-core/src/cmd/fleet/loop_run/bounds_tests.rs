use super::*;

fn bounds_fixture(name: &str) -> Fleet {
    Fleet {
        name: name.into(),
        display_name: String::new(),
        goal: "g".into(),
        router: None,
        team_id: None,
        members: vec!["pm".into()],
        channel_id: format!("fleet-{name}"),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    }
}

/// Guards, narrowest reason first: deadline, then stuck, then the
/// diagnostic ceiling. Stuck is a CLOCK now — how long since progress —
/// not a count of iterations.
#[test]
fn guards_are_deadline_then_stuck_then_ceiling() {
    use mur_common::limits::Stuck;
    let m = Duration::from_secs(60);
    assert_eq!(
        check_guards(
            0,
            Duration::ZERO,
            60 * m,
            Duration::ZERO,
            Stuck::After(10 * m)
        ),
        None
    );
    assert_eq!(
        check_guards(3, 61 * m, 60 * m, Duration::ZERO, Stuck::After(10 * m)),
        Some(LoopStop::Deadline)
    );
    assert_eq!(
        check_guards(3, 5 * m, 60 * m, 10 * m, Stuck::After(10 * m)),
        Some(LoopStop::Stuck)
    );
    assert_eq!(
        check_guards(3, 5 * m, 60 * m, 99 * m, Stuck::Off),
        None,
        "off never trips"
    );
    assert_eq!(
        check_guards(
            LOOP_ITERATION_CEILING,
            5 * m,
            60 * m,
            Duration::ZERO,
            Stuck::Off
        ),
        Some(LoopStop::MaxIterations)
    );
    assert_eq!(
        check_guards(3, 61 * m, 60 * m, 10 * m, Stuck::After(10 * m)),
        Some(LoopStop::Deadline),
        "deadline beats stuck when both are due"
    );
}

/// §3.3 + §3.4: a fleet with no limits: block gets the 1h built-in;
/// flags beat fleet.yaml beats config.yaml; a legacy loop.budget_usd is
/// the cost cap (applicability is decided later by `budget_for`).
#[test]
fn fleet_bounds_resolve_across_scopes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::write(
        home.join("config.yaml"),
        "limits:\n  deadline: 4h\n  stuck: 20m\n",
    )
    .unwrap();
    let mut f = bounds_fixture("dev");
    f.loop_cfg = Some(mur_common::fleet::FleetLoop {
        trigger: "manual".into(),
        max_iterations: 8,
        budget_usd: 5.0,
        deadline: "2h".into(),
        done_when: String::new(),
    });
    let b = fleet_bounds(home, &f, None, None).unwrap();
    assert_eq!(b.deadline, Duration::from_secs(2 * 3600));
    assert_eq!(b.deadline_source, mur_common::limits::Source::Fleet);
    assert_eq!(
        b.stuck,
        mur_common::limits::Stuck::After(Duration::from_secs(20 * 60))
    );
    assert_eq!(b.cost_usd, Some(5.0));
    let b = fleet_bounds(home, &f, Some("15m"), Some(1.0)).unwrap();
    assert_eq!(b.deadline, Duration::from_secs(15 * 60));
    assert_eq!(b.deadline_source, mur_common::limits::Source::Flag);
    assert_eq!(b.cost_usd, Some(1.0));
    f.loop_cfg = None;
    std::fs::remove_file(home.join("config.yaml")).unwrap();
    let b = fleet_bounds(home, &f, None, None).unwrap();
    assert_eq!(b.deadline, mur_common::limits::DEFAULT_DEADLINE_FLEET);
    assert_eq!(b.deadline_source, mur_common::limits::Source::BuiltIn);
    // an unparsable value is an error that names the key, not a default
    f.limits = Some(mur_common::limits::Limits {
        deadline: Some("soon".into()),
        stuck: None,
        cost_usd: None,
    });
    let e = fleet_bounds(home, &f, None, None).unwrap_err().to_string();
    assert!(e.contains("limits.deadline") && e.contains("soon"), "{e}");
}

/// The remedies name the commands that exist now.
#[test]
fn remedies_name_mur_fleet_limits() {
    assert!(
        stop_remedy(LoopStop::Deadline, "dev")
            .unwrap()
            .contains("mur fleet limits dev --deadline")
    );
    assert!(
        stop_remedy(LoopStop::Stuck, "dev")
            .unwrap()
            .contains("--stuck")
    );
    assert!(
        stop_remedy(LoopStop::Budget, "dev")
            .unwrap()
            .contains("--cost-usd")
    );
    assert!(
        stop_remedy(LoopStop::MaxIterations, "dev")
            .unwrap()
            .contains("ceiling")
    );
}

/// The whole loop is one run: a record exists while it runs and ends in
/// the state its stop implies, so `mur_job_status <run_id>` answers for a
/// handle the caller minted before the loop started.
#[tokio::test]
async fn the_loop_records_itself_under_the_callers_run_id() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let mut f = bounds_fixture("dev");
    f.loop_cfg = Some(mur_common::fleet::FleetLoop {
        trigger: "manual".into(),
        max_iterations: 0,
        budget_usd: 0.0,
        deadline: String::new(),
        done_when: "queue-empty".into(),
    });
    crate::cmd::fleet::store::save_fleet(home, &f).unwrap();
    mur_channel::ChannelService::open(home)
        .unwrap()
        .create_for_fleet(
            "dev",
            "mur",
            &["pm".into()],
            mur_channel::service::ChannelOwner::Legacy,
        )
        .unwrap();
    let (stop, _, _) = run_guarded(
        home,
        "dev",
        None,
        None,
        None,
        Some("fleet-dev-abc".into()),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(stop, LoopStop::QueueDrained);
    let rec = crate::run_status::store::load(home, "fleet-dev-abc")
        .unwrap()
        .expect("recorded");
    assert_eq!(rec.kind, crate::run_status::RunKind::Fleet);
    assert_eq!(rec.state, crate::run_status::State::Done);
    assert!(rec.last_heartbeat_at.is_some());
    let status = crate::run_status::status_of(home, "fleet-dev-abc")
        .unwrap()
        .unwrap();
    assert_eq!(status.state, crate::run_status::State::Done);
    let e = run_guarded(
        home,
        "dev",
        None,
        None,
        None,
        Some("bad id!".into()),
        None,
        None,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("invalid --run-id"), "{e}");
}

/// A per-run goal reaches the run without touching `fleet.yaml`: the
/// progress record carries it, and the fleet's standing goal is unchanged
/// on disk. `mur deep-research "<q>"` rewrote fleet.yaml here, which an
/// agent-triggered run cannot do (its sandbox leaves `fleets/` read-only).
#[tokio::test]
async fn a_goal_override_is_this_runs_goal_and_leaves_fleet_yaml_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let mut f = bounds_fixture("dev");
    f.loop_cfg = Some(mur_common::fleet::FleetLoop {
        trigger: "manual".into(),
        max_iterations: 0,
        budget_usd: 0.0,
        deadline: String::new(),
        done_when: "queue-empty".into(),
    });
    crate::cmd::fleet::store::save_fleet(home, &f).unwrap();
    let before = std::fs::read(crate::cmd::fleet::store::fleet_path(home, "dev")).unwrap();
    mur_channel::ChannelService::open(home)
        .unwrap()
        .create_for_fleet(
            "dev",
            "mur",
            &["pm".into()],
            mur_channel::service::ChannelOwner::Legacy,
        )
        .unwrap();
    let (stop, _, _) = run_guarded(
        home,
        "dev",
        None,
        None,
        None,
        None,
        Some("what changed in 2.91?".into()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(stop, LoopStop::QueueDrained);
    let (progress, _) = super::super::progress::load(home, "dev").expect("progress");
    assert_eq!(progress.question, "what changed in 2.91?");
    let after = std::fs::read(crate::cmd::fleet::store::fleet_path(home, "dev")).unwrap();
    assert_eq!(before, after, "a run must not rewrite fleet.yaml");
}

#[test]
fn stops_map_to_run_states() {
    use crate::run_status::State;
    assert_eq!(loop_terminal_state(LoopStop::Converged), State::Done);
    assert_eq!(loop_terminal_state(LoopStop::QueueDrained), State::Done);
    assert_eq!(
        loop_terminal_state(LoopStop::AwaitingApproval),
        State::Blocked
    );
    assert_eq!(loop_terminal_state(LoopStop::Stopped), State::Stopped);
    assert_eq!(
        loop_terminal_state(LoopStop::CommanderKilled),
        State::Stopped
    );
    for s in [
        LoopStop::Deadline,
        LoopStop::Stuck,
        LoopStop::Budget,
        LoopStop::MaxIterations,
    ] {
        assert_eq!(loop_terminal_state(s), State::Failed, "{s:?}");
    }
}
