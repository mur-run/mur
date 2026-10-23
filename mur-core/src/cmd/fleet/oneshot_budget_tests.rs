use super::*;

fn ev(agent: &str, kind: StepEventKind, tokens: u64) -> StepEvent {
    StepEvent {
        id: "s".into(),
        agent: Some(agent.into()),
        kind,
        tokens_used: tokens,
        error: None,
    }
}

/// $1/1k tokens, $5 cap; `api` is metered, `local` and `sub` are not.
fn budget() -> Arc<OneShotBudget> {
    let bills = HashMap::from([
        ("api".to_string(), true),
        ("local".to_string(), false),
        ("sub".to_string(), false),
    ]);
    Arc::new(OneShotBudget::new(5.0, 1.0, bills))
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
