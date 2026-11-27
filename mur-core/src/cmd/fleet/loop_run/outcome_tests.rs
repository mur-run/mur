use super::*;

#[test]
fn iteration_goal_drains_queue_then_falls_back_to_standing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::cmd::fleet::roster::touch_agents_for_test(home, &["pm", "mur"]);
    super::super::create::cmd_fleet_create(
        home,
        "dev",
        vec!["pm".into()],
        None,
        Some("standing".into()),
        None,
    )
    .unwrap();

    // empty queue → standing goal, no job
    let (g, j) = iteration_goal(home, "dev", "standing").unwrap();
    assert_eq!(g, "standing");
    assert!(j.is_none());

    // queued job → job text, marked running
    super::super::jobs::enqueue_job(home, "dev", "job-1", "cli").unwrap();
    let (g, j) = iteration_goal(home, "dev", "standing").unwrap();
    assert_eq!(g, "job-1");
    assert_eq!(j.unwrap().status, mur_common::fleet::JobStatus::Running);
}

/// Test seam: run one guarded iteration and return the stop reason.
pub(super) async fn run_loop_for_test(home: &Path) -> LoopStop {
    run_guarded(home, "dev", Some(1), None, None, None, None, None)
        .await
        .map(|(stop, _, _)| stop)
        .unwrap_or(LoopStop::MaxIterations)
}

#[tokio::test]
async fn progress_file_written_with_outcome_on_guard_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "research question".into(),
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
    // Kill-switch: the loop stops before any delegation, so no live agent
    // is needed — but the before-loop and exit progress writes still run.
    crate::cmd::fleet::control::cmd_fleet_stop(home, "dev").unwrap();

    let stop = run_loop_for_test(home).await;
    assert_eq!(stop, LoopStop::Stopped);

    let (p, _) = crate::cmd::fleet::progress::load(home, "dev").expect("progress file written");
    assert_eq!(p.schema_version, 1);
    assert_eq!(p.question, "research question");
    assert!(p.finished_at.is_some());
    assert_eq!(p.outcome.as_deref(), Some("stopped"));
}

/// C: a fleet whose every member failed must not be recorded `done`.
///
/// Observed 2026-09-20: three deep-research workers each failed, the
/// iteration returned `PipelineStatus::Failed`, the loop read only
/// `Skipped` (blocked) and fell through to synthesis, the router emitted
/// the convergence marker, and `Converged` mapped to `State::Done`. The
/// run record said done while nothing had been researched.
#[test]
fn an_iteration_that_failed_is_not_a_converged_fleet() {
    use crate::run_status::State;
    assert_eq!(
        loop_terminal_state(LoopStop::IterationFailed),
        State::Failed,
        "a fleet whose iteration failed must record `failed`"
    );
    assert_eq!(terminal_state_for(LoopStop::IterationFailed), "failed");
    assert_eq!(outcome_label(LoopStop::IterationFailed), "iteration-failed");
    // It is a real failure, so it owes the user a way out.
    let remedy = stop_remedy(LoopStop::IterationFailed, "dev")
        .expect("a failed iteration must name a remedy");
    assert!(
        remedy.contains("dev"),
        "remedy must name the fleet: {remedy}"
    );
    // And it must not be scored against triage as an overrun — the work
    // failed, the guards did not trip.
    assert_eq!(calibration_stop_reason(LoopStop::IterationFailed), None);
}

/// Every stop has a remedy except the two that mean "done". The remedy
/// names a command that exists today; the spec's later steps rewrite it.
#[test]
fn every_stop_short_of_done_names_a_remedy() {
    for stop in [
        LoopStop::MaxIterations,
        LoopStop::Deadline,
        LoopStop::Stuck,
        LoopStop::Budget,
        LoopStop::Stopped,
        LoopStop::CommanderKilled,
        LoopStop::AwaitingApproval,
        LoopStop::IterationFailed,
    ] {
        let r = stop_remedy(stop, "dev").unwrap_or_else(|| panic!("{stop:?} has no remedy"));
        assert!(
            r.contains("dev"),
            "{stop:?}: remedy must name the fleet: {r}"
        );
    }
    assert_eq!(stop_remedy(LoopStop::Converged, "dev"), None);
    assert_eq!(stop_remedy(LoopStop::QueueDrained, "dev"), None);
    assert!(
        stop_remedy(LoopStop::MaxIterations, "dev")
            .unwrap()
            .contains("ceiling")
    );
    assert!(
        stop_remedy(LoopStop::Deadline, "dev")
            .unwrap()
            .contains("--deadline")
    );
    assert!(
        stop_remedy(LoopStop::Budget, "dev")
            .unwrap()
            .contains("--cost-usd")
    );
    assert!(
        stop_remedy(LoopStop::Stopped, "dev")
            .unwrap()
            .contains("mur fleet start dev")
    );
    assert!(
        stop_remedy(LoopStop::AwaitingApproval, "dev")
            .unwrap()
            .contains("mur channel approve")
    );
}

/// The map from "why we stopped" to the channel's terminal state. Done is
/// completed; a kill is canceled; waiting on a person is input-required;
/// a guard trip is failed — the goal was not reached.
#[test]
fn stop_maps_to_a_channel_terminal_state() {
    assert_eq!(terminal_state_for(LoopStop::Converged), "completed");
    assert_eq!(terminal_state_for(LoopStop::QueueDrained), "completed");
    assert_eq!(terminal_state_for(LoopStop::Stopped), "canceled");
    assert_eq!(terminal_state_for(LoopStop::CommanderKilled), "canceled");
    assert_eq!(
        terminal_state_for(LoopStop::AwaitingApproval),
        "input-required"
    );
    for stop in [
        LoopStop::MaxIterations,
        LoopStop::Deadline,
        LoopStop::Stuck,
        LoopStop::Budget,
    ] {
        assert_eq!(terminal_state_for(stop), "failed", "{stop:?}");
    }
}

/// The stop reaches the channel, not only progress.json: one System
/// state-change at the end of the run carrying reason and remedy.
#[tokio::test]
async fn a_guard_stop_is_written_to_the_channel_with_reason_and_remedy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
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
    let svc = mur_channel::ChannelService::open(home).unwrap();
    svc.create_for_fleet(
        "dev",
        "mur",
        &["pm".into()],
        mur_channel::service::ChannelOwner::Legacy,
    )
    .unwrap();
    // Kill-switch: the loop stops before any delegation, no live agent needed.
    crate::cmd::fleet::control::cmd_fleet_stop(home, "dev").unwrap();

    let stop = run_loop_for_test(home).await;
    assert_eq!(stop, LoopStop::Stopped);

    let events = svc.load_events("fleet-dev").unwrap();
    let last = events
        .last()
        .expect("the stop event is the last thing written");
    assert_eq!(last.kind, mur_common::channel::EventKind::StateChange);
    assert_eq!(last.actor, ChannelActor::System);
    assert_eq!(last.payload["to"], "canceled");
    assert_eq!(last.payload["stop_reason"], "stopped");
    assert_eq!(last.payload["remedy"], "cleared by: mur fleet start dev");
    assert!(
        last.payload["run_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
}

/// A local or subscription fleet has no spend, so a `budget_usd` on it is
/// noise: the guard would stop a run on a projection of dollars nobody is
/// paying. Unknown billing keeps the budget — conservative, like the fold.
/// A billable fleet on a deadline alone is allowed, but told: the notice
/// names the bound and the rate, offers the cap, and stays silent for a
/// fleet that cannot spend or one that already has a cap.
#[test]
fn a_billable_fleet_without_a_cap_is_told_so() {
    use super::super::billing::FleetBilling;
    let local = FleetBilling {
        billable: false,
        unknown: vec![],
    };
    let billed = FleetBilling {
        billable: true,
        unknown: vec![],
    };
    let two_h = Some(Duration::from_secs(7200));
    let n = no_cap_notice(&billed, None, two_h, 0.05).expect("billable, no cap → notice");
    assert!(
        n.contains("7200s") && n.contains("$0.05/1k") && n.contains("--budget-usd"),
        "{n}"
    );
    assert!(
        no_cap_notice(&billed, Some(5.0), two_h, 0.05).is_none(),
        "capped → silent"
    );
    assert!(
        no_cap_notice(&local, None, two_h, 0.05).is_none(),
        "cannot spend → silent"
    );
    assert!(
        no_cap_notice(&billed, None, None, 0.05)
            .unwrap()
            .contains("iteration cap")
    );
}

#[test]
fn a_budget_applies_only_to_a_fleet_that_can_spend() {
    use super::super::billing::FleetBilling;
    let local = FleetBilling {
        billable: false,
        unknown: vec![],
    };
    let billed = FleetBilling {
        billable: true,
        unknown: vec![],
    };
    assert_eq!(budget_for(Some(5.0), &billed), Some(5.0));
    assert_eq!(budget_for(Some(5.0), &local), None);
    assert_eq!(budget_for(None, &billed), None);
    assert_eq!(budget_for(None, &local), None);
}
