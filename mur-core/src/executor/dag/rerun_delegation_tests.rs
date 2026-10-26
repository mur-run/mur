//! A delegate step dispatched again under the same `run_id` (a resumed run,
//! or a crash-rerun of a step whose result was never recorded) must leave its
//! OWN `Delegation` event on the channel. The delegation key is derived only
//! from `(channel, run_id, step_id, attempt)`, so a second dispatch with
//! attempt 0 collides with the first and the dedup-aware writer drops it —
//! the channel keeps pointing at the first, abandoned child task.

use super::*;

#[tokio::test]
async fn redispatching_a_delegate_step_records_a_second_delegation() {
    use mur_channel::ChannelService;
    use mur_common::channel::EventKind;

    let tmp = tempfile::TempDir::new().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("rerun-wf").unwrap();

    // The member is not running, so the dial fails AFTER the Delegation event
    // is appended: the step never records a ToolResult and the resume cursor
    // lets the second run dispatch it again — exactly the resumed-run path.
    let procedure = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("d0".to_string()),
            description: "delegated step".to_string(),
            delegate_to: Some("absent-member".to_string()),
            ..Default::default()
        }],
    };
    let opts = DagExecOptions {
        channel_id: Some(ch.id.clone()),
        run_id: "run-rerun".into(),
        yes: true,
        ..Default::default()
    };

    for _ in 0..2 {
        let _ = execute_dag(tmp.path(), "rerun-wf", &procedure, &opts).await;
    }

    let child_tasks: Vec<String> = svc
        .load_events(&ch.id)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::Delegation)
        .filter_map(|e| e.payload["child_task_id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        child_tasks.len(),
        2,
        "each dispatch must record its own Delegation; got {child_tasks:?}"
    );
    assert_ne!(child_tasks[0], child_tasks[1], "distinct child tasks");
}
