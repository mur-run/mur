use super::*;
use mur_common::model::BillingMode;

fn ev(agent: &str, kind: StepEventKind, tokens: u64) -> StepEvent {
    StepEvent {
        id: "s".into(),
        agent: Some(agent.into()),
        kind,
        usage: TokenUsage {
            input: tokens,
            output: 0,
            model_ref: None,
        },
        error: None,
    }
}

fn model(billing: BillingMode, input: Option<f64>, output: Option<f64>) -> ModelEntry {
    ModelEntry {
        billing: Some(billing),
        input_cost_per_1k: input,
        output_cost_per_1k: output,
        ..Default::default()
    }
}

fn agent(bills: bool, model_ref: Option<&str>) -> AgentBilling {
    AgentBilling {
        bills,
        model_ref: model_ref.map(str::to_string),
    }
}

/// $5 cap, $1/1k fallback. `api` runs an unpriced metered model (so it bills
/// at the fallback), `local` and `sub` are not billed, `router` is the
/// fleet's planner on a metered model priced $1/1k in, $1/1k out.
fn budget() -> Arc<OneShotBudget> {
    let agents = HashMap::from([
        ("api".to_string(), agent(true, Some("unpriced"))),
        ("local".to_string(), agent(false, Some("ollama"))),
        ("sub".to_string(), agent(false, Some("codex"))),
        ("router".to_string(), agent(true, Some("router-model"))),
    ]);
    let models = BTreeMap::from([
        (
            "unpriced".to_string(),
            model(BillingMode::UsageBilled, None, None),
        ),
        ("ollama".to_string(), model(BillingMode::Local, None, None)),
        (
            "codex".to_string(),
            model(BillingMode::Subscription, None, None),
        ),
        (
            "router-model".to_string(),
            model(BillingMode::UsageBilled, Some(1.0), Some(1.0)),
        ),
    ]);
    Arc::new(OneShotBudget::new(
        5.0,
        1.0,
        "router".into(),
        agents,
        models,
    ))
}

#[test]
fn trips_on_actual_billable_spend_and_halts_the_run() {
    let b = budget();
    let stop = b.stop_signal(Arc::new(|| false));
    b.record(&ev("api", StepEventKind::Done, 3_000));
    assert!(!b.tripped() && !stop(), "$3 of $5 is under the cap");
    b.record(&ev("api", StepEventKind::Failed, 2_000));
    assert!(
        b.tripped() && stop(),
        "$5 reaches the cap and halts the DAG"
    );
    assert!(b.stop_reason().starts_with(BUDGET_STOP_REASON));
}

#[test]
fn local_and_subscription_members_never_count() {
    let b = budget();
    b.record(&ev("local", StepEventKind::Done, 1_000_000));
    b.record(&ev("sub", StepEventKind::Done, 1_000_000));
    assert_eq!(b.spent_usd(), 0.0);
    assert!(!b.tripped(), "tokens nobody pays for must not stop the run");
}

#[test]
fn an_unresolvable_agent_counts_as_metered() {
    let b = budget();
    b.record(&ev("stranger", StepEventKind::Done, 5_000));
    assert!(b.tripped());
}

#[test]
fn non_terminal_events_carry_no_spend() {
    let b = budget();
    b.record(&ev("api", StepEventKind::Started, 9_000));
    b.record(&ev("api", StepEventKind::Blocked, 9_000));
    assert_eq!(b.spent_usd(), 0.0);
}

#[test]
fn the_kill_switch_still_stops_an_untripped_budget() {
    let b = budget();
    let stop = b.stop_signal(Arc::new(|| true));
    assert!(
        stop() && !b.tripped(),
        "the stop is the operator's, not the cap's"
    );
}

/// End to end through the real executor: the cap trips when rank 0 reports
/// its spend, rank 1 never starts, and the run ends `Stopped` — the same
/// terminal path as `mur fleet stop` (#2036). Command steps report no usage,
/// so the observer stands in for a metered member's delegate turn.
#[tokio::test]
async fn reaching_the_cap_stops_the_dag_before_the_next_rank() {
    use crate::executor::dag::{DagExecOptions, execute_dag};
    use mur_common::pipeline::PipelineStatus;
    use mur_common::skill::manifest::{Procedure, ProcedureStep};

    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("rank1-ran");
    let step = |id: &str, deps: &[&str], cmd: String| ProcedureStep {
        id: Some(id.to_string()),
        command: Some(cmd),
        description: format!("{id} step"),
        depends_on: deps.iter().map(|d| d.to_string()).collect(),
        ..Default::default()
    };
    let proc = Procedure {
        variables: vec![],
        steps: vec![
            step("s0", &[], "true".into()),
            step("s1", &["s0"], format!("touch {}", marker.display())),
        ],
    };
    let b = budget();
    let feed = {
        let b = Arc::clone(&b);
        Arc::new(move |e: StepEvent| {
            if e.id == "s0" && e.kind == StepEventKind::Done {
                b.record(&ev("api", StepEventKind::Done, 6_000));
            }
        })
    };
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            stop: Some(b.stop_signal(Arc::new(|| false))),
            on_step: Some(feed),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(b.tripped());
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(!marker.exists(), "no step may start past the cap");
}

/// Negative control: the same run on local members spends nothing and
/// finishes — the cap must not stop work nobody is billed for.
#[tokio::test]
async fn the_same_run_on_local_members_finishes() {
    use crate::executor::dag::{DagExecOptions, execute_dag};
    use mur_common::pipeline::PipelineStatus;
    use mur_common::skill::manifest::{Procedure, ProcedureStep};

    let tmp = tempfile::TempDir::new().unwrap();
    let proc = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("s0".into()),
            command: Some("true".into()),
            description: "s0".into(),
            ..Default::default()
        }],
    };
    let b = budget();
    let feed = {
        let b = Arc::clone(&b);
        Arc::new(move |e: StepEvent| {
            if e.kind == StepEventKind::Done {
                b.record(&ev("local", StepEventKind::Done, 6_000));
            }
        })
    };
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            stop: Some(b.stop_signal(Arc::new(|| false))),
            on_step: Some(feed),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(!b.tripped());
    assert_eq!(out.status, PipelineStatus::Success);
}

/// Review of #2086: each turn is billed at its own model's input and output
/// rates. The old guard billed every token at the dearest output rate in the
/// whole registry, so cheap input tokens counted as expensive output ones.
#[test]
fn a_turn_is_priced_at_its_own_models_input_and_output_rates() {
    let agents = HashMap::from([("api".to_string(), agent(true, Some("cheap")))]);
    let models = BTreeMap::from([
        (
            "cheap".to_string(),
            model(BillingMode::UsageBilled, Some(0.001), Some(0.004)),
        ),
        // Never used by this fleet: it must not set the rate.
        (
            "opus".to_string(),
            model(BillingMode::UsageBilled, Some(0.015), Some(0.075)),
        ),
    ]);
    let b = OneShotBudget::new(1.0, 0.075, "router".into(), agents, models);
    b.record_turn(
        Some("api"),
        &TokenUsage {
            input: 100_000,
            output: 10_000,
            model_ref: Some("cheap".into()),
        },
    );
    // 100k in at $0.001 + 10k out at $0.004 = $0.14, not 110k at $0.075.
    assert!((b.spent_usd() - 0.14).abs() < 1e-9, "{}", b.spent_usd());
    assert!(
        !b.tripped(),
        "an unrelated dear model must not trip the cap"
    );
}

/// The model the runtime says answered the turn beats the profile's: a
/// fallback chain can route a "local" agent to a metered model.
#[test]
fn the_reported_model_beats_the_profile_model() {
    let b = budget();
    b.record_turn(
        Some("local"),
        &TokenUsage {
            input: 3_000,
            output: 2_000,
            model_ref: Some("router-model".into()),
        },
    );
    assert!((b.spent_usd() - 5.0).abs() < 1e-9);
    assert!(b.tripped());
}

/// Review of #2086: the router's planning turn is billable work of the run.
/// A metered router over local members must still be able to hit the cap.
#[test]
fn a_metered_router_counts_even_when_every_member_is_local() {
    let b = budget();
    b.record_router(&TokenUsage {
        input: 4_000,
        output: 1_000,
        model_ref: None,
    });
    assert!((b.spent_usd() - 5.0).abs() < 1e-9);
    assert!(b.tripped(), "the router alone reached the cap");
    b.record(&ev("local", StepEventKind::Done, 1_000_000));
    assert!((b.spent_usd() - 5.0).abs() < 1e-9, "members stay free");
}

/// End to end: the router's turn reaches the cap during planning, so the DAG
/// halts at its first step boundary — no member step starts at all.
#[tokio::test]
async fn a_router_that_reached_the_cap_stops_the_run_before_any_step() {
    use crate::executor::dag::{DagExecOptions, execute_dag};
    use mur_common::pipeline::PipelineStatus;
    use mur_common::skill::manifest::{Procedure, ProcedureStep};

    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("member-ran");
    let proc = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("s0".into()),
            command: Some(format!("touch {}", marker.display())),
            description: "s0".into(),
            ..Default::default()
        }],
    };
    let b = budget();
    b.record_router(&TokenUsage {
        input: 5_000,
        output: 1_000,
        model_ref: None,
    });
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            stop: Some(b.stop_signal(Arc::new(|| false))),
            on_step: Some(b.observer()),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(!marker.exists(), "no member step may start past the cap");
    assert!(b.stop_reason().starts_with(BUDGET_STOP_REASON));
}
