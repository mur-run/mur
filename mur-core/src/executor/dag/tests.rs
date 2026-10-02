use super::*;

fn result(success: bool, exit_code: i32, out: &str) -> StepResult {
    StepResult {
        exit_code,
        output_text: out.to_string(),
        duration_ms: 0,
        failed_step: None,
        success,
        blocked: false,
        tokens_used: 0,
    }
}

#[test]
fn a_failed_step_always_has_a_reason_and_a_done_step_never_does() {
    assert_eq!(step_failure_reason(&result(true, 0, "fine")), None);
    assert_eq!(
        step_failure_reason(&result(false, 1, "  delegate failed: refused  ")).as_deref(),
        Some("delegate failed: refused")
    );
    // Silence is itself the finding: six steps once failed with nothing
    // recorded at all, and a bare exit code beats an empty field.
    assert_eq!(
        step_failure_reason(&result(false, 7, "   ")).as_deref(),
        Some("no output; exit code 7")
    );
}

#[test]
fn truncation_survives_multibyte_output() {
    // `String::truncate` panics off a char boundary, and agent output is
    // routinely CJK — the old byte cap was a live panic in the executor.
    let cjk = "調度中".repeat(400);
    let cut = truncate_chars(&cjk, STEP_ERROR_MAX_CHARS);
    assert_eq!(
        cut.chars().count(),
        STEP_ERROR_MAX_CHARS + 1,
        "cap + ellipsis"
    );
    assert!(cut.ends_with('…'));
    assert_eq!(truncate_chars("short", STEP_ERROR_MAX_CHARS), "short");
}

use mur_common::skill::manifest::ProcedureStep;

fn step(id: &str, deps: &[&str], cmd: Option<&str>) -> ProcedureStep {
    ProcedureStep {
        id: Some(id.to_string()),
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
        command: cmd.map(|s| s.to_string()),
        description: format!("step {id}"),
        ..Default::default()
    }
}

#[test]
fn idem_key_is_deterministic_and_distinct() {
    let a = idem_key("chan", "run", "s0", "delegate");
    let b = idem_key("chan", "run", "s0", "delegate");
    let c = idem_key("chan", "run", "s0", "reply");
    assert_eq!(a, b, "same inputs → same key (crash-rerun stable)");
    assert_ne!(a, c, "different suffix → different key");
    assert_eq!(a.len(), 64, "sha256 hex");
}

#[test]
fn channel_delegate_params_thread_goal_channel_task_and_idem_key() {
    // v3d-2: the concierge delegates via `channel/delegate`, threading the
    // channel id + the deterministic reply_key (as idempotency_key) so the
    // specialist signs its OWN reply Message and re-dials fold.
    let p = build_channel_delegate_params(
        "find the bug",
        "chan-1",
        "child-1",
        "rk-deadbeef",
        None,
        &[],
    );
    assert_eq!(p["channel_id"], "chan-1");
    assert_eq!(p["task_id"], "child-1");
    assert_eq!(p["idempotency_key"], "rk-deadbeef");
    assert_eq!(p["message"]["role"], "user");
    let text = p["message"]["parts"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("find the bug"));
    assert!(text.contains("Completion:"));
}

#[test]
fn thread_dep_outputs_appends_completed_dependency_outputs() {
    let mut s = step("s3", &["s1", "s2"], None);
    s.delegate_to = Some("dr_worker_3".into());
    s.intent = Some("synthesize the brief".into());
    let outputs: HashMap<String, String> = [
        ("s1".to_string(), "claims + citations".to_string()),
        ("s2".to_string(), "CONFIRM x3".to_string()),
    ]
    .into();
    thread_dep_outputs(&mut s, &outputs);
    let intent = s.intent.unwrap();
    assert!(intent.starts_with("synthesize the brief"));
    assert!(intent.contains("[Outputs from completed dependency steps]"));
    assert!(intent.contains("--- output of dependency step s1 ---\nclaims + citations"));
    assert!(intent.contains("--- output of dependency step s2 ---\nCONFIRM x3"));
}

#[test]
fn thread_dep_outputs_noop_without_delegate_or_deps_or_outputs() {
    // Non-delegated step: untouched even with completed deps.
    let mut cmd_step = step("s1", &["s0"], Some("echo hi"));
    let outputs: HashMap<String, String> = [("s0".to_string(), "out".to_string())].into();
    thread_dep_outputs(&mut cmd_step, &outputs);
    assert!(cmd_step.intent.is_none());
    // Delegated step whose deps produced nothing: intent unchanged.
    let mut d = step("s2", &["s9"], None);
    d.delegate_to = Some("w".into());
    d.intent = Some("go".into());
    thread_dep_outputs(&mut d, &outputs);
    assert_eq!(d.intent.as_deref(), Some("go"));
}

#[test]
fn dep_output_excerpt_truncates_on_char_boundary() {
    let s = "研".repeat(DEP_OUTPUT_EXCERPT_MAX); // 3 bytes per char
    let e = dep_output_excerpt(&s);
    assert!(e.len() <= DEP_OUTPUT_EXCERPT_MAX + "\n…[truncated]".len());
    assert!(e.ends_with("…[truncated]"));
    // Short input passes through untouched.
    assert_eq!(dep_output_excerpt("ok"), "ok");
}

/// The bug this whole seam exists for: a turn a runtime guard aborted
/// still ends with a summary message, and `success = !reply.is_empty()`
/// scored that as a clean `done`. Observed 2026-09-13 — 95K tokens, 54
/// iterations, no code written, `"state": "done"`, three dispatches spent.
#[test]
fn a_guard_stopped_delegate_is_not_a_success() {
    let task = serde_json::json!({
        "id": "t1",
        "state": "completed",
        "messages": [
            {"role":"user","parts":[{"kind":"text","text":"do task 4"}]},
            {"role":"agent","parts":[{"kind":"text","text":"Task 4 is blocked."}]}
        ],
        "usage": {"input_tokens": 95000, "output_tokens": 200,
                  "stop_reason": "loop_detected", "iterations": 54}
    });
    let r = delegate_result(&task, "task 4", 1);
    assert!(!r.success, "a loop-aborted turn must not score as success");
    assert_eq!(r.exit_code, 1);
    assert_eq!(r.failed_step.as_deref(), Some("task 4"));
    assert!(
        r.output_text.contains("loop_detected after 54 iterations"),
        "the verdict must say which guard fired: {}",
        r.output_text
    );
    assert!(
        r.output_text.contains("Task 4 is blocked."),
        "the specialist's own words must survive: {}",
        r.output_text
    );
    // Not `blocked`: that means "waiting on a human, nothing ran", and it
    // skips the ledger + on_failure. This turn ran and spent 95K tokens.
    assert!(!r.blocked);
    assert_eq!(r.tokens_used, 95_200, "spend is still accounted");
}

/// A runtime that refuses to start reports WHY in `Task.error.message`
/// and writes no agent message at all. Reading only `messages` + `usage`
/// left `output_text` empty, and the ledger's fallback rendered the most
/// useless sentence in the codebase: `no output; exit code 1`. Observed
/// 2026-09-20 — three deep-research workers failed `cannot_start` and the
/// reason was sitting in a field nothing read.
#[test]
fn a_delegate_error_reason_survives_into_the_verdict() {
    let task = serde_json::json!({
        "id": "t1",
        "state": "failed",
        "messages": [
            {"role":"user","parts":[{"kind":"text","text":"research it"}]}
        ],
        "error": {
            "code": "cannot_start",
            "message": "cannot start: specialist has no write_file — mur agent perm tool-allow specialist write_file"
        }
    });
    let r = delegate_result(&task, "research it", 1);
    assert!(!r.success);
    assert_eq!(r.exit_code, 1);
    assert!(
        r.output_text.contains("cannot_start"),
        "the error code must reach the ledger: {:?}",
        r.output_text
    );
    assert!(
        r.output_text.contains("mur agent perm tool-allow"),
        "the runtime's remedy must survive verbatim: {:?}",
        r.output_text
    );
    // The whole point: never again `no output; exit code 1`.
    assert_ne!(
        step_failure_reason(&r).as_deref(),
        Some("no output; exit code 1"),
        "a known error must never degrade to the empty-output fallback"
    );
}

/// The error must not shove aside words the specialist did manage to say,
/// and must not fire when the runtime reported no error at all.
#[test]
fn delegate_error_joins_the_reply_and_is_absent_when_clean() {
    let with_both = serde_json::json!({
        "id": "t1",
        "state": "failed",
        "messages": [
            {"role":"agent","parts":[{"kind":"text","text":"got halfway"}]}
        ],
        "error": {"code": "tool_denied", "message": "bash is not allowed"}
    });
    let r = delegate_result(&with_both, "s", 1);
    assert!(!r.success, "an error field alone means the turn failed");
    assert!(r.output_text.contains("tool_denied"), "{}", r.output_text);
    assert!(
        r.output_text.contains("got halfway"),
        "partial work must survive: {}",
        r.output_text
    );
    // Null / missing error must not manufacture a failure.
    let clean = serde_json::json!({
        "id": "t1",
        "state": "completed",
        "error": serde_json::Value::Null,
        "messages": [
            {"role":"agent","parts":[{"kind":"text","text":"all done"}]}
        ]
    });
    let ok = delegate_result(&clean, "s", 1);
    assert!(ok.success, "a null error is not an error");
    assert_eq!(ok.output_text, "all done");
}

/// Negative control: without a guard stop the old contract still holds, so
/// the assertion above is about `stop_reason` and not a broken extractor.
#[test]
fn a_clean_delegate_reply_is_still_a_success() {
    let task = serde_json::json!({
        "id": "t1",
        "state": "completed",
        "messages": [
            {"role":"user","parts":[{"kind":"text","text":"do task 4"}]},
            {"role":"agent","parts":[{"kind":"text","text":"Done; tests pass."}]}
        ],
        "usage": {"input_tokens": 10, "output_tokens": 5}
    });
    let r = delegate_result(&task, "task 4", 1);
    assert!(r.success);
    assert_eq!(r.exit_code, 0);
    assert!(r.failed_step.is_none());
    assert_eq!(
        r.output_text, "Done; tests pass.",
        "no note on a clean turn"
    );
}

/// An empty reply was already a failure; it must stay one.
#[test]
fn an_empty_delegate_reply_is_still_a_failure() {
    let task = serde_json::json!({"id": "t1", "state": "completed", "messages": []});
    let r = delegate_result(&task, "task 4", 1);
    assert!(!r.success);
    assert_eq!(r.exit_code, 1);
}

/// Every guard the runtime can apply to itself, not just the one observed.
#[test]
fn every_guard_stop_reason_fails_the_step() {
    for reason in ["loop_detected", "stuck", "deadline", "iteration_ceiling"] {
        let task = serde_json::json!({
            "id": "t1",
            "messages": [{"role":"agent","parts":[{"kind":"text","text":"summary"}]}],
            "usage": {"stop_reason": reason}
        });
        let r = delegate_result(&task, "s", 1);
        assert!(!r.success, "{reason} must fail the step");
        assert!(r.output_text.contains(reason), "{reason} must be named");
    }
}

#[test]
fn extract_agent_reply_takes_last_agent_message() {
    let task = serde_json::json!({
        "id": "t1",
        "messages": [
            {"role":"user","parts":[{"kind":"text","text":"q"}]},
            {"role":"agent","parts":[{"kind":"text","text":"partial "},{"kind":"text","text":"answer"}]}
        ]
    });
    assert_eq!(extract_agent_reply(&task), "partial answer");
    // No agent message → empty.
    let empty = serde_json::json!({ "messages": [{"role":"user","parts":[]}] });
    assert_eq!(extract_agent_reply(&empty), "");
}

#[test]
fn linear_chain_toposorts() {
    let steps = vec![
        step("s0", &[], Some("echo zero")),
        step("s1", &["s0"], Some("echo one")),
        step("s2", &["s1"], Some("echo two")),
    ];
    let graph = build_dag(&steps).unwrap();
    assert_eq!(graph.nodes.len(), 3);
    // s0 rank 0, s1 rank 1, s2 rank 2
    for n in &graph.nodes {
        let id = n.step.id.as_deref().unwrap();
        let expected = match id {
            "s0" => 0,
            "s1" => 1,
            "s2" => 2,
            _ => unreachable!(),
        };
        assert_eq!(n.rank, expected, "step {id} expected rank {expected}");
    }
}

#[test]
fn concurrent_roots() {
    let steps = vec![
        step("s0", &[], None), // root
        step("s1", &[], None), // root
        step("s2", &["s0", "s1"], None),
    ];
    let graph = build_dag(&steps).unwrap();
    for n in &graph.nodes {
        match n.step.id.as_deref().unwrap() {
            "s0" | "s1" => assert_eq!(n.rank, 0),
            "s2" => assert_eq!(n.rank, 1),
            _ => unreachable!(),
        }
    }
}

#[test]
fn cycle_detected() {
    let steps = vec![step("s0", &["s1"], None), step("s1", &["s0"], None)];
    let err = build_dag(&steps).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("cycle"), "expected cycle error, got: {msg}");
}

#[test]
fn unknown_dep_detected() {
    let steps = vec![step("s0", &["nonexistent"], None)];
    let err = build_dag(&steps).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("nonexistent"),
        "expected nonexistent dep error, got: {msg}"
    );
}

#[test]
fn missing_ids_autogenerated() {
    let steps = vec![ProcedureStep {
        id: None,
        depends_on: vec![],
        command: Some("echo hi".to_string()),
        ..Default::default()
    }];
    let graph = build_dag(&steps).unwrap();
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.nodes[0].step.id.as_deref(), Some("s0"));
}

#[test]
fn empty_steps_returns_ok() {
    let tmp = tempfile::TempDir::new().unwrap();
    let proc = Procedure {
        variables: vec![],
        steps: vec![],
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let out = rt
        .block_on(execute_dag(
            tmp.path(),
            "empty-test",
            &proc,
            &DagExecOptions::default(),
        ))
        .unwrap();
    assert_eq!(out.exit_code, 0);
}

// channel_run_refuses_needs_approval removed: v3c gates via hitl::gate instead
// of refusing. See high_risk_step_gates_and_runs_when_preapproved below.

#[tokio::test]
async fn on_step_observer_sees_start_and_done() {
    use std::sync::{Arc, Mutex};

    let proc = Procedure {
        variables: vec![],
        steps: vec![
            step("s1", &[], Some("echo one")),
            step("s2", &[], Some("echo two")),
        ],
    };
    let seen: Arc<Mutex<Vec<(String, StepEventKind)>>> = Arc::new(Mutex::new(vec![]));
    let sink = seen.clone();
    let opts = DagExecOptions {
        on_step: Some(Arc::new(move |e: StepEvent| {
            sink.lock().unwrap().push((e.id, e.kind));
        })),
        ..Default::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let _ = execute_dag(tmp.path(), "test", &proc, &opts).await.unwrap();
    let seen = seen.lock().unwrap();
    assert!(seen.contains(&("s1".into(), StepEventKind::Started)));
    assert!(seen.contains(&("s1".into(), StepEventKind::Done)));
    assert!(seen.contains(&("s2".into(), StepEventKind::Done)));
}

#[tokio::test]
async fn resume_skips_a_step_already_completed() {
    use mur_channel::ChannelService;
    use mur_common::channel::EventKind;

    let tmp = tempfile::TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("resume-wf").unwrap();

    let proc = Procedure {
        variables: vec![],
        steps: vec![
            step("s0", &[], Some("echo zero")),
            step("s1", &["s0"], Some("echo one")),
        ],
    };
    let opts = DagExecOptions {
        channel_id: Some(ch.id.clone()),
        run_id: "run-1".into(),
        yes: true,
        ..Default::default()
    };
    execute_dag(tmp.path(), "resume-wf", &proc, &opts)
        .await
        .unwrap();
    let after_first = svc.load_events(&ch.id).unwrap().len();

    execute_dag(tmp.path(), "resume-wf", &proc, &opts)
        .await
        .unwrap();
    let tr_after_second = svc
        .load_events(&ch.id)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::ToolResult)
        .count();
    assert_eq!(
        tr_after_second, 2,
        "rerun did not duplicate completed-step results"
    );
    let _ = after_first;
}

#[tokio::test]
async fn high_risk_step_gates_and_runs_when_preapproved() {
    use mur_channel::ChannelService;
    use mur_common::channel::EventKind;
    use mur_common::hitl::RiskTier;

    let tmp = tempfile::TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("gated-wf").unwrap();
    let mut s = step("s0", &[], Some("echo done"));
    s.risk = Some(RiskTier::Destructive);
    let proc = Procedure {
        variables: vec![],
        steps: vec![s],
    };
    let opts = DagExecOptions {
        channel_id: Some(ch.id.clone()),
        run_id: "run-1".into(),
        yes: true,
        ..Default::default()
    };
    let out = execute_dag(tmp.path(), "gated-wf", &proc, &opts)
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);
    let kinds: Vec<_> = svc
        .load_events(&ch.id)
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect();
    assert!(
        kinds.contains(&EventKind::HitlRequest),
        "high-risk step raised a gate"
    );
}

#[test]
fn channel_run_emits_attributed_event_trail() {
    use mur_channel::ChannelService;
    use mur_common::channel::{ChannelActor, ChannelState, EventKind};

    let tmp = tempfile::TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("test-skill").unwrap();

    let proc = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("s0".to_string()),
            command: Some("echo hi".to_string()),
            description: "echo step".to_string(),
            ..Default::default()
        }],
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(execute_dag(
        tmp.path(),
        "test-skill",
        &proc,
        &DagExecOptions {
            channel_id: Some(ch.id.clone()),
            ..DagExecOptions::default()
        },
    ))
    .unwrap();

    let evs = svc.load_events(&ch.id).unwrap();
    let kinds: Vec<_> = evs.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds.first(),
        Some(&EventKind::StateChange),
        "first event must be StateChange(Working)"
    );
    assert_eq!(
        kinds.last(),
        Some(&EventKind::StateChange),
        "last event must be StateChange(Completed)"
    );
    assert_eq!(
        evs.iter().filter(|e| e.kind == EventKind::ToolCall).count(),
        1,
        "one ToolCall per step"
    );
    assert_eq!(
        evs.iter()
            .filter(|e| e.kind == EventKind::ToolResult)
            .count(),
        1,
        "one ToolResult per step"
    );
    assert!(
        evs.iter().all(|e| e.actor == ChannelActor::System),
        "all events must have actor=System"
    );
    assert_eq!(
        svc.store().load_manifest(&ch.id).unwrap().state,
        ChannelState::Completed
    );
    let tr = evs
        .iter()
        .find(|e| e.kind == EventKind::ToolResult)
        .unwrap();
    assert_eq!(tr.payload["exit_code"], 0);
}

#[tokio::test]
async fn max_concurrency_bounds_parallel_steps() {
    // 6 independent rank-0 steps. Each registers itself in a shared `run/`
    // dir, records how many steps are concurrently registered, sleeps, then
    // deregisters. The MAX recorded count is the observed peak concurrency —
    // a deterministic property of the executor's semaphore, NOT a wall-clock
    // measurement, so it does not flake on slow/loaded CI runners the way the
    // old timing-ratio assertion did (chronically red on Windows/macOS).

    // Build a procedure whose steps probe live concurrency into `probe_dir`.
    // Forward-slash paths so the `sh -c` body works under Git Bash on Windows.
    fn probe_proc(probe_dir: &str) -> Procedure {
        Procedure {
            variables: vec![],
            steps: (0..6)
                .map(|i| ProcedureStep {
                    description: format!("s{i}"),
                    command: Some(format!(
                        "mkdir -p '{probe_dir}/run'; : > '{probe_dir}/run/{i}'; \
                             ls '{probe_dir}/run' | wc -l >> '{probe_dir}/peaks'; \
                             sleep 0.2; rm -f '{probe_dir}/run/{i}'"
                    )),
                    id: Some(format!("s{i}")),
                    ..Default::default()
                })
                .collect(),
        }
    }
    // Highest concurrency the steps observed (max line in `peaks`).
    fn observed_peak(probe_dir: &std::path::Path) -> usize {
        std::fs::read_to_string(probe_dir.join("peaks"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse::<usize>().ok())
            .max()
            .unwrap_or(0)
    }
    fn fwd(p: &std::path::Path) -> String {
        p.display().to_string().replace('\\', "/")
    }

    // Bounded to 2 -> the executor's semaphore guarantees <= 2 concurrent.
    let tmp_b = tempfile::TempDir::new().unwrap();
    let probe_b = tempfile::TempDir::new().unwrap();
    let opts = DagExecOptions {
        max_concurrency: Some(2),
        ..Default::default()
    };
    execute_dag(
        tmp_b.path(),
        "cc-bounded",
        &probe_proc(&fwd(probe_b.path())),
        &opts,
    )
    .await
    .unwrap();
    let bounded_peak = observed_peak(probe_b.path());

    // Unbounded -> all 6 run in a single wave.
    let tmp_u = tempfile::TempDir::new().unwrap();
    let probe_u = tempfile::TempDir::new().unwrap();
    let opts2 = DagExecOptions {
        max_concurrency: None,
        ..Default::default()
    };
    execute_dag(
        tmp_u.path(),
        "cc-unbounded",
        &probe_proc(&fwd(probe_u.path())),
        &opts2,
    )
    .await
    .unwrap();
    let unbounded_peak = observed_peak(probe_u.path());

    assert!(
        bounded_peak <= 2,
        "bounded peak {bounded_peak} exceeded the max_concurrency cap of 2"
    );
    assert!(
        unbounded_peak >= 3,
        "unbounded peak {unbounded_peak} should exceed the cap (cap not lifted / steps not parallel)"
    );
}

/// A run with an id must be observable from disk while it executes, and
/// must land on a terminal state when it finishes. Without this, a
/// timeout is the only signal a caller ever gets — which is the defect.
#[tokio::test]
async fn execute_dag_records_and_finalizes_a_run() {
    use crate::run_status::store;

    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let procedure = Procedure {
        variables: vec![],
        steps: vec![step("s1", &[], None)],
    };
    let opts = DagExecOptions {
        run_id: "run-under-test".into(),
        run_kind: Some(crate::run_status::RunKind::Workflow),
        run_label: "test run".into(),
        ..Default::default()
    };

    let _ = execute_dag(mur_home, "test-skill", &procedure, &opts).await;

    let run = store::load(mur_home, "run-under-test")
        .unwrap()
        .expect("execute_dag never wrote run.json");
    assert_eq!(run.run_id, "run-under-test");
    assert_eq!(
        run.pid,
        std::process::id(),
        "must record the orchestrator pid"
    );
    assert!(
        run.state.is_terminal(),
        "run left non-terminal after execute_dag returned: {:?}",
        run.state
    );
}

/// THE regression for the review's empty-steps finding: the record is
/// written with `steps: []` and nothing ever updates it, so `mur job
/// status` cannot answer "what is it doing now?". Drive the real
/// executor over a one-step procedure and assert the FINAL record's
/// steps reflect the lifecycle the `on_step` observer saw — Started
/// stamped, then Done, with both timestamps set. The point is that
/// steps are no longer empty.
#[tokio::test]
async fn recorded_run_steps_reflect_the_step_lifecycle() {
    use crate::run_status::{State, store};

    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let procedure = Procedure {
        variables: vec![],
        steps: vec![step("s1", &[], Some("echo hi"))],
    };
    let opts = DagExecOptions {
        run_id: "run-with-steps".into(),
        run_kind: Some(crate::run_status::RunKind::Workflow),
        run_label: "steps run".into(),
        ..Default::default()
    };

    let _ = execute_dag(mur_home, "test-skill", &procedure, &opts).await;

    let run = store::load(mur_home, "run-with-steps")
        .unwrap()
        .expect("execute_dag never wrote run.json");
    assert_eq!(
        run.steps.len(),
        1,
        "the record's steps were never updated — `mur job status` would \
             show no step rows for a step that ran"
    );
    let step0 = &run.steps[0];
    assert_eq!(step0.id, "s1", "step id must be the DAG step id");
    assert_eq!(
        step0.state,
        State::Done,
        "the final record must show the step done"
    );
    assert!(
        step0.started_at.is_some() && step0.ended_at.is_some(),
        "both lifecycle timestamps must be stamped: {step0:?}"
    );
}

/// A run-status recording failure (e.g. an unwritable `~/.mur/runs/`)
/// must not fail the run itself — observability must never take down
/// the thing it observes. Force `store::save`'s `create_dir_all` to fail
/// deterministically by putting a plain file where the run's directory
/// needs to go, then prove `execute_dag` still runs its step to a
/// successful conclusion instead of propagating the I/O error.
#[tokio::test]
async fn execute_dag_survives_a_run_recording_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let run_id = "run-that-cannot-be-recorded";

    // `store::save` does `create_dir_all(<mur_home>/runs/<run_id>)`; a
    // regular file already sitting at that exact path makes that call
    // fail every time, deterministically, with no timing dependency.
    std::fs::create_dir_all(mur_home.join("runs")).unwrap();
    std::fs::write(mur_home.join("runs").join(run_id), b"not a directory").unwrap();

    let procedure = Procedure {
        variables: vec![],
        steps: vec![step("s1", &[], None)],
    };
    let opts = DagExecOptions {
        run_id: run_id.into(),
        run_kind: Some(crate::run_status::RunKind::Workflow),
        run_label: "test run".into(),
        ..Default::default()
    };

    let output = execute_dag(mur_home, "test-skill", &procedure, &opts)
        .await
        .expect(
            "execute_dag returned Err — a run-status recording failure \
                 propagated out of the executor instead of being logged and \
                 ignored, so bookkeeping took down real work",
        );
    assert_eq!(
        output.status,
        PipelineStatus::Success,
        "execute_dag did not complete its step after a run-status \
             recording failure — bookkeeping is taking down real work"
    );
}

/// An empty `run_id` is the legacy default. It must not create a
/// directory called "" under runs/.
#[tokio::test]
async fn execute_dag_without_a_run_id_records_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let procedure = Procedure {
        variables: vec![],
        steps: vec![step("s1", &[], None)],
    };
    let opts = DagExecOptions::default();

    let _ = execute_dag(tmp.path(), "test-skill", &procedure, &opts).await;

    assert!(
        crate::run_status::store::list_ids(tmp.path())
            .unwrap()
            .is_empty(),
        "recorded a run for an empty run_id"
    );
}

/// Pin the executor's own `sidecar.json` write — the `save_sidecar` call
/// in `execute_dag`'s recording block. A store-level round-trip test
/// would pass even if the executor never called it, and then a corrupt
/// run.json would silently hide the run: exactly the defect the
/// run-status fallback exists to remove, recreated as a testing blind
/// spot. This test runs the real executor against a real channel,
/// corrupts the cache it just wrote, and proves `status_of` re-derives
/// the record from the channel through the sidecar the executor left
/// behind.
#[tokio::test]
async fn corrupt_run_json_falls_back_via_the_executors_channel_sidecar() {
    use mur_channel::ChannelService;

    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let svc = ChannelService::open(mur_home).unwrap();
    let ch = svc.create_for_workflow("sidecar-wf").unwrap();

    let procedure = Procedure {
        variables: vec![],
        steps: vec![step("s1", &[], Some("echo done"))],
    };
    let opts = DagExecOptions {
        run_id: "run-sidecar".into(),
        run_kind: Some(crate::run_status::RunKind::Workflow),
        run_label: "sidecar test run".into(),
        channel_id: Some(ch.id.clone()),
        ..Default::default()
    };
    execute_dag(mur_home, "sidecar-wf", &procedure, &opts)
        .await
        .expect("execute_dag failed");

    // Corrupt the cache the executor just wrote. The sidecar must survive
    // it and carry the rebuild.
    let run_json = crate::run_status::store::run_path(mur_home, "run-sidecar");
    assert!(run_json.exists(), "execute_dag never wrote run.json");
    std::fs::write(&run_json, b"{ this is not json").unwrap();

    let status = crate::run_status::status_of(mur_home, "run-sidecar")
        .unwrap()
        .expect("the executor's sidecar must make a corrupt cache fall back to the channel");
    assert_eq!(
        status.state,
        crate::run_status::State::Done,
        "the channel's Completed transition must be re-derived from its events"
    );
    assert!(
        status.run.last_heartbeat_at.is_none(),
        "a re-derived record must report an unknown heartbeat, never invent one"
    );
}

/// §3.4: the delegate carries the fleet's REMAINING clock, never a fresh one.
#[test]
fn delegate_params_carry_remaining_deadline() {
    let p = build_channel_delegate_params("do x", "fleet-dev", "t1", "k1", Some(720), &[]);
    assert_eq!(p["limits"]["deadline_secs"], 720);
    let p = build_channel_delegate_params("do x", "fleet-dev", "t1", "k1", None, &[]);
    assert!(
        p.get("limits").is_none(),
        "no clock → the member resolves its own scopes"
    );
}

#[test]
fn delegate_params_carry_the_fleets_needs() {
    let p = build_channel_delegate_params(
        "do x",
        "fleet-dev",
        "t1",
        "k1",
        None,
        &["write_file".into(), "bash".into()],
    );
    assert_eq!(p["needs"], serde_json::json!(["write_file", "bash"]));
    let p = build_channel_delegate_params("do x", "fleet-dev", "t1", "k1", None, &[]);
    assert!(
        p.get("needs").is_none(),
        "no needs → no key → no preflight (old behaviour)"
    );
}

#[test]
fn remaining_secs_floors_at_one_and_is_none_without_a_deadline() {
    let now = std::time::Instant::now();
    assert_eq!(remaining_secs(None, now), None);
    assert_eq!(
        remaining_secs(Some(now + std::time::Duration::from_secs(90)), now),
        Some(90)
    );
    assert_eq!(
        remaining_secs(Some(now), now),
        Some(1),
        "a delegate that starts at the deadline gets one second, not zero"
    );
}
