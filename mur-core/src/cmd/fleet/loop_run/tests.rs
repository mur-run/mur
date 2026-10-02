use super::*;

fn agent_event(seq: u64, id: &str, text: &str) -> ChannelEvent {
    ChannelEvent {
        seq,
        ts: chrono::Utc::now(),
        actor: ChannelActor::Agent { id: id.into() },
        kind: mur_common::channel::EventKind::Note,
        payload: serde_json::json!({ "text": text }),
        idempotency_key: None,
        sig: None,
        key_version: None,
    }
}

fn sys_event(seq: u64, text: &str) -> ChannelEvent {
    ChannelEvent {
        seq,
        ts: chrono::Utc::now(),
        actor: ChannelActor::System,
        kind: mur_common::channel::EventKind::Note,
        payload: serde_json::json!({ "text": text }),
        idempotency_key: None,
        sig: None,
        key_version: None,
    }
}

#[test]
fn final_synthesis_always_has_exactly_one_terminal_marker() {
    assert_eq!(
        finalize_synthesis("answer", "RESEARCH_COMPLETE"),
        "answer\n\nRESEARCH_COMPLETE"
    );
    assert_eq!(
        finalize_synthesis("answer\nRESEARCH_COMPLETE\n", "RESEARCH_COMPLETE"),
        "answer\nRESEARCH_COMPLETE"
    );
}

#[test]
fn synthesis_prompt_requires_report_and_own_line_marker() {
    let prompt = build_synthesis_prompt("What changed?", "RESEARCH_COMPLETE", "worker evidence");
    assert!(prompt.contains("What changed?"));
    assert!(prompt.contains("worker evidence"));
    assert!(prompt.contains("final cited answer"));
    assert!(prompt.contains("\nRESEARCH_COMPLETE\n"));
    assert!(prompt.contains("last non-blank line"));
}

#[test]
fn channel_has_marker_matches_member_events_after_baseline() {
    let evs = vec![
        // System goal event MENTIONS the token — must NOT self-trigger.
        sys_event(1, "goal: emit DONE_TOKEN when finished"),
        agent_event(2, "qa", "still working"),
        agent_event(3, "pm", "all green\nDONE_TOKEN"), // sentinel on its own line
    ];
    // a member emitted the marker as a sentinel after baseline 0 → converged
    assert!(channel_has_marker(&evs, "DONE_TOKEN", 0));
    // baseline at seq 3 excludes this-run events → not converged (stale-run guard)
    assert!(!channel_has_marker(&evs, "DONE_TOKEN", 3));
    // the System goal event alone never counts (Agent-authored only)
    assert!(!channel_has_marker(&evs[..1], "DONE_TOKEN", 0));
    // absent marker
    assert!(!channel_has_marker(&evs, "NOT_PRESENT", 0));
}

#[test]
fn channel_has_marker_rejects_prose_mentions_of_the_marker() {
    // The marker is fanned out to members in the goal, so prose that quotes
    // or negates it must NOT converge — only a deliberate own-line sentinel.
    let planning = vec![agent_event(
        2,
        "qa",
        "I will emit DONE_TOKEN when tests pass",
    )];
    assert!(!channel_has_marker(&planning, "DONE_TOKEN", 0));
    let negated = vec![agent_event(2, "qa", "DONE_TOKEN not yet emitted")];
    assert!(!channel_has_marker(&negated, "DONE_TOKEN", 0));
    let embedded = vec![agent_event(2, "qa", "see ABANDONED_TOKENS below")];
    assert!(!channel_has_marker(&embedded, "DONE_TOKEN", 0));
    // but a trailing-whitespace sentinel line still converges (trimmed)
    let sentinel = vec![agent_event(2, "qa", "done:\n  DONE_TOKEN  ")];
    assert!(channel_has_marker(&sentinel, "DONE_TOKEN", 0));
}

#[test]
fn parse_duration_units_and_bare_seconds() {
    assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
    assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
    assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86_400)));
    assert_eq!(parse_duration(" 2h "), Some(Duration::from_secs(7200)));
    assert_eq!(parse_duration(""), None);
    assert_eq!(parse_duration("abc"), None);
    assert_eq!(parse_duration("2y"), None);
    assert_eq!(parse_duration("h"), None);
}

#[test]
fn is_converged_detects_done_word_only() {
    assert!(is_converged("DONE"));
    assert!(is_converged("done"));
    assert!(is_converged("The goal is DONE."));
    assert!(is_converged("Done — all issues closed"));
    assert!(!is_converged("CONTINUE"));
    assert!(!is_converged("not done yet")); // negation guard → continue
    assert!(!is_converged("done but CONTINUE")); // continue token wins
    assert!(!is_converged("undone")); // substring, not a token → false
    assert!(!is_converged(""));
}

/// The regression this constant exists for. A measured deep-research run —
/// three members on a $0.003/1k model — cost $21.52, and the old estimate
/// (8000 tokens per member) projected about $0.07, so a $10 ceiling waved
/// it through and reported the overrun only afterwards.
///
/// The projection must now exceed a $10 ceiling for that shape, so the loop
/// refuses BEFORE spending rather than apologising after.
#[test]
fn a_three_member_iteration_no_longer_projects_as_pocket_change() {
    let projected = estimate_iteration_cost_usd(3, 0.003);
    assert!(
        projected > 10.0,
        "three members must not project under a $10 ceiling: ${projected}"
    );
    // …and not so dear that the shipped default refuses a normal run.
    assert!(
        projected < crate::cmd::deep_research::setup::DEFAULT_RUN_BUDGET_USD,
        "the default budget must still admit one iteration: ${projected}"
    );
    assert!(budget_exceeded(0.0, projected, Some(10.0)));
    assert!(!budget_exceeded(
        0.0,
        projected,
        Some(crate::cmd::deep_research::setup::DEFAULT_RUN_BUDGET_USD)
    ));
}

#[test]
fn estimate_and_budget_guard() {
    // Expressed through the constant, not a literal: this pins the FORMULA
    // (linear in members and in price), and the constant is documented as
    // something to retune once there is more than one measured run.
    let per_member_k = EST_TOKENS_PER_MEMBER_ITERATION as f64 / 1000.0;
    assert!((estimate_iteration_cost_usd(2, 0.05) - 2.0 * per_member_k * 0.05).abs() < 1e-9);
    // Linear in both arguments.
    assert!(
        (estimate_iteration_cost_usd(4, 0.05) - 2.0 * estimate_iteration_cost_usd(2, 0.05)).abs()
            < 1e-9
    );
    assert!(
        (estimate_iteration_cost_usd(2, 0.10) - 2.0 * estimate_iteration_cost_usd(2, 0.05)).abs()
            < 1e-9
    );
    // no budget / zero budget → never blocks
    assert!(!budget_exceeded(100.0, 5.0, None));
    assert!(!budget_exceeded(100.0, 5.0, Some(0.0)));
    // stops BEFORE exceeding
    assert!(budget_exceeded(0.9, 0.2, Some(1.0))); // 1.1 > 1.0
    assert!(!budget_exceeded(0.7, 0.2, Some(1.0))); // 0.9 <= 1.0
    // even iteration 0 unaffordable → blocked immediately
    assert!(budget_exceeded(0.0, 2.0, Some(1.0)));
}

#[test]
fn real_iteration_cost_from_tokens() {
    // 10_000 tokens × $0.05/1k = $0.50
    assert!((iteration_cost_usd(10_000, 0.05) - 0.5).abs() < 1e-9);
    // zero tokens → zero (caller falls back to the projection to avoid
    // under-counting; the helper itself is exact).
    assert_eq!(iteration_cost_usd(0, 0.05), 0.0);
    // real cost is typically well under the 8000-tok/member projection:
    // a 1200-token iteration costs far less than the 1-member projection.
    assert!(iteration_cost_usd(1200, 0.05) < estimate_iteration_cost_usd(1, 0.05));
}

/// The mapping is worthless if it emits a name the calibration does not
/// recognise, so it is checked against the real constant rather than
/// against a copy of the strings.
#[test]
fn every_mapped_stop_reason_is_one_the_calibration_scores() {
    use crate::executor::triage_calibration::GUARD_STOPS;
    for stop in [
        LoopStop::Converged,
        LoopStop::MaxIterations,
        LoopStop::Deadline,
        LoopStop::Stuck,
        LoopStop::Budget,
        LoopStop::Stopped,
        LoopStop::CommanderKilled,
        LoopStop::QueueDrained,
        LoopStop::AwaitingApproval,
    ] {
        if let Some(r) = calibration_stop_reason(stop) {
            assert!(
                GUARD_STOPS.contains(&r),
                "{stop:?} maps to `{r}`, which GUARD_STOPS does not contain —                      the calibration would silently read it as no overrun"
            );
        }
    }
}

/// The rename trap: the loop says `max-iterations`, the runtime says
/// `iteration_ceiling`. Passing the human label straight through would
/// lose every runaway triage was meant to predict.
#[test]
fn a_runaway_maps_to_the_runtimes_name_not_the_human_label() {
    assert_eq!(
        calibration_stop_reason(LoopStop::MaxIterations),
        Some("iteration_ceiling")
    );
    assert_ne!(
        calibration_stop_reason(LoopStop::MaxIterations),
        Some(outcome_label(LoopStop::MaxIterations)),
        "the display label must not be what gets scored"
    );
}

/// A run that ended well must never be scored as an overrun — that would
/// blame triage for letting through work that was fine.
#[test]
fn a_clean_finish_is_not_an_overrun() {
    for stop in [
        LoopStop::Converged,
        LoopStop::QueueDrained,
        LoopStop::Stopped,
        LoopStop::CommanderKilled,
        LoopStop::AwaitingApproval,
    ] {
        assert_eq!(calibration_stop_reason(stop), None, "{stop:?}");
    }
}

/// Budget is detected from spend vs cap, not from a guard name the
/// runtime never emits.
#[test]
fn a_budget_stop_is_left_to_the_spend_comparison() {
    assert_eq!(calibration_stop_reason(LoopStop::Budget), None);
}

#[test]
fn fleet_price_per_1k_env_then_default() {
    let tmp = tempfile::tempdir().unwrap();
    // env override wins (nextest isolates per-process, so this is safe)
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_FLEET_COST_PER_1K", "0.123");
    let (rate, src) = fleet_price_per_1k(tmp.path());
    assert!((rate - 0.123).abs() < 1e-9);
    assert_eq!(src, GuardRate::Env);
    // no env + no models.yaml → documented default, and it says so
    envg.unset_var("MUR_FLEET_COST_PER_1K");
    let (rate, src) = fleet_price_per_1k(tmp.path());
    assert!((rate - DEFAULT_PRICE_PER_1K).abs() < 1e-9);
    assert_eq!(src, GuardRate::Default);
}

/// The budget guard bills one flat rate against an iteration's whole token
/// count, while the cost report prices each component separately. If the
/// guard's rate ever dips below what the report would charge for a token of
/// any model in the registry, the fleet overspends its budget by exactly
/// that ratio — and nothing on screen says so. Both bugs this branch fixed
/// were that failure: a rate in the wrong field made the guard 5x cheap,
/// and a stripped registry would have made it 57x cheap.
#[test]
fn guard_rate_is_never_below_what_the_report_charges() {
    use crate::cmd::conversations_cost_report::resolve_rates;
    use mur_common::model::{ModelEntry, ModelRegistry};

    let entry = |input: f64, output: f64| ModelEntry {
        provider: "anthropic".into(),
        model: "m".into(),
        input_cost_per_1k: Some(input),
        output_cost_per_1k: Some(output),
        ..Default::default()
    };
    let mut reg = ModelRegistry::default();
    for (alias, wire, i, o) in [
        ("opus", "claude-opus-5", 0.005, 0.025),
        ("sonnet", "claude-sonnet-5", 0.003, 0.015),
        ("haiku", "claude-haiku-4-5", 0.001, 0.005),
        ("ds", "deepseek-v4-pro", 0.000435, 0.00087),
    ] {
        reg.models.insert(
            alias.into(),
            ModelEntry {
                model: wire.into(),
                ..entry(i, o)
            },
        );
    }

    let guard = dearest_output_rate(&reg).expect("registry is priced");
    for e in reg.models.values() {
        // Per 1k, against the report's per-1M rates.
        let ((r_in, r_out, r_cw, r_cr), _) =
            resolve_rates(&e.model, Some(&reg)).expect("report prices it");
        for (label, report_rate) in [
            ("input", r_in),
            ("output", r_out),
            ("cache_write", r_cw),
            ("cache_read", r_cr),
        ] {
            assert!(
                guard >= report_rate / 1000.0,
                "guard {guard}/1k is under the {label} rate the report charges for {} \
                     ({}/1k) — a fleet on that model overspends its budget",
                e.model,
                report_rate / 1000.0
            );
        }
    }
}

/// A registry that prices nothing must not collapse the ceiling to zero —
/// the documented default is deliberately dearer than any real model.
#[test]
fn unpriced_registry_yields_no_ceiling_rather_than_a_free_one() {
    use mur_common::model::{ModelEntry, ModelRegistry};
    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "local".into(),
        ModelEntry {
            provider: "openai".into(),
            model: "Qwen3.5-4B-MLX-4bit".into(),
            ..Default::default()
        },
    );
    assert_eq!(dearest_output_rate(&reg), None);
    // Const item: editing the default below a real model's rate fails the
    // BUILD, not just this test.
    const _: () = assert!(DEFAULT_PRICE_PER_1K > 0.025);
}

#[test]
fn iteration_goal_drains_queue_then_falls_back_to_standing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
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
async fn run_loop_for_test(home: &Path) -> LoopStop {
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
        .create_for_fleet("dev", "mur", &["pm".into()])
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
    svc.create_for_fleet("dev", "mur", &["pm".into()]).unwrap();
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
        .create_for_fleet("dev", "mur", &["pm".into()])
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
        .create_for_fleet("dev", "mur", &["pm".into()])
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
        .create_for_fleet("dev", "mur", &["pm".into()])
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
        .create_for_fleet("dev", "mur", &["pm".into()])
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
        .create_for_fleet("dev", "mur", &["pm".into()])
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
        .create_for_fleet("dev", "mur", &["pm".into()])
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
        .create_for_fleet("dev", "mur", &["pm".into()])
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
