//! #1613: a `delegate_to` step run without a channel calls no member. It still
//! succeeds (a preview is legitimate), but the run record must say `skipped`
//! with the reason — never `done` for work nobody did.

use super::step::{UNDELEGATED_REASON, is_undelegated};
use super::*;

fn step(id: &str, cmd: Option<&str>) -> ProcedureStep {
    ProcedureStep {
        id: Some(id.to_string()),
        command: cmd.map(|s| s.to_string()),
        description: format!("step {id}"),
        ..Default::default()
    }
}

#[tokio::test]
async fn channelless_delegate_step_is_recorded_skipped_not_done() {
    use crate::run_status::{State, store};

    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let mut delegated = step("d1", None);
    delegated.delegate_to = Some("rustsmith".into());
    let procedure = Procedure {
        variables: vec![],
        steps: vec![delegated, step("c1", Some("echo hi"))],
    };
    let opts = DagExecOptions {
        run_id: "run-undelegated".into(),
        run_kind: Some(crate::run_status::RunKind::Workflow),
        run_label: "undelegated run".into(),
        ..Default::default()
    };

    let out = execute_dag(mur_home, "test-skill", &procedure, &opts)
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0, "a preview run must not fail");

    let run = store::load(mur_home, "run-undelegated").unwrap().unwrap();
    let d1 = run.steps.iter().find(|s| s.id == "d1").unwrap();
    assert_eq!(d1.state, State::Skipped, "{d1:?}");
    assert_eq!(d1.error.as_deref(), Some(UNDELEGATED_REASON));
    assert!(d1.ended_at.is_some(), "skipped is terminal: {d1:?}");
    let c1 = run.steps.iter().find(|s| s.id == "c1").unwrap();
    assert_eq!(c1.state, State::Done, "a plain step is untouched: {c1:?}");
    assert!(c1.error.is_none());
}

#[test]
fn only_a_channelless_commandless_delegate_step_is_undelegated() {
    let mut s = step("d1", None);
    let none = DagExecOptions::default();
    assert!(!is_undelegated(&s, &none), "no delegate_to");
    s.delegate_to = Some("rustsmith".into());
    assert!(is_undelegated(&s, &none));
    let with_channel = DagExecOptions {
        channel_id: Some("ch-1".into()),
        ..Default::default()
    };
    assert!(!is_undelegated(&s, &with_channel), "has a channel");
    s.command = Some("echo hi".into());
    assert!(!is_undelegated(&s, &none), "a command really ran");
}
