use super::*;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::Content;
use mur_common::workflow::{Step, Workflow};
use tempfile::TempDir;

fn make_store(tmp: &TempDir) -> WorkflowYamlStore {
    WorkflowYamlStore::new(tmp.path().to_path_buf()).unwrap()
}

fn make_workflow(name: &str, steps: Vec<Step>) -> Workflow {
    Workflow {
        base: KnowledgeBase {
            name: name.to_string(),
            description: format!("Test workflow {}", name),
            content: Content::Plain(String::new()),
            ..Default::default()
        },
        steps,
        variables: vec![],
        source_sessions: vec![],
        trigger: String::new(),
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    }
}

fn shell_step(order: u32, desc: &str, cmd: &str) -> Step {
    Step {
        order,
        description: desc.to_string(),
        command: Some(cmd.to_string()),
        ..Default::default()
    }
}

fn prompt_step(order: u32, desc: &str) -> Step {
    Step {
        order,
        description: desc.to_string(),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_shell_step_captures_stdout() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    let wf = make_workflow(
        "echo-test",
        vec![shell_step(1, "Echo hello", "echo hello-world")],
    );
    store.save(&wf).unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Single("echo-test".into());
    let output = executor.execute(&expr, None).await.unwrap();

    assert_eq!(output.status, PipelineStatus::Success);
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.output_text.as_deref(), Some("hello-world"));
}

#[tokio::test]
async fn test_input_injection_in_command() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    let wf = make_workflow(
        "injector",
        vec![shell_step(1, "Echo input", "echo {{input}}")],
    );
    store.save(&wf).unwrap();

    let executor = PipelineExecutor::new(store);
    let piped = PipelineOutput {
        workflow_id: "prev".into(),
        status: PipelineStatus::Success,
        output_text: Some("INJECTED_DATA".into()),
        output_data: None,
        exit_code: 0,
        duration_ms: 0,
        tokens_used: 0,
    };

    let expr = PipelineExpr::Single("injector".into());
    let output = executor.execute(&expr, Some(piped)).await.unwrap();

    assert_eq!(output.status, PipelineStatus::Success);
    assert_eq!(output.output_text.as_deref(), Some("INJECTED_DATA"));
}

#[tokio::test]
async fn test_input_injection_in_prompt_step() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    let wf = make_workflow("prompt-inject", vec![prompt_step(1, "Analyze: {{input}}")]);
    store.save(&wf).unwrap();

    let executor = PipelineExecutor::new(store);
    let piped = PipelineOutput {
        workflow_id: "prev".into(),
        status: PipelineStatus::Success,
        output_text: Some("some data".into()),
        output_data: None,
        exit_code: 0,
        duration_ms: 0,
        tokens_used: 0,
    };

    let expr = PipelineExpr::Single("prompt-inject".into());
    let output = executor.execute(&expr, Some(piped)).await.unwrap();

    let text = output.output_text.as_deref().unwrap();
    assert!(
        text == "Analyze: 'some data'" || text == "Analyze: \"some data\"",
        "unexpected: {text}"
    );
}

#[tokio::test]
async fn test_pipe_chains_output() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    // w1: produces "hello"
    store
        .save(&make_workflow(
            "w1",
            vec![shell_step(1, "Produce", "echo hello")],
        ))
        .unwrap();

    // w2: receives {{input}}, transforms it
    store
        .save(&make_workflow(
            "w2",
            vec![shell_step(1, "Transform", "echo got-{{input}}")],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Pipe(
        Box::new(PipelineExpr::Single("w1".into())),
        Box::new(PipelineExpr::Single("w2".into())),
    );

    let output = executor.execute(&expr, None).await.unwrap();
    assert_eq!(output.status, PipelineStatus::Success);
    // w1 outputs "hello", w2 gets it as {{input}} → "got-hello"
    assert_eq!(output.output_text.as_deref(), Some("got-hello"));
}

#[tokio::test]
async fn test_pipe_stops_on_failure() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    store
        .save(&make_workflow(
            "fail-w",
            vec![shell_step(1, "Fail", "exit 1")],
        ))
        .unwrap();
    store
        .save(&make_workflow(
            "never-run",
            vec![shell_step(1, "Should not run", "echo nope")],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Pipe(
        Box::new(PipelineExpr::Single("fail-w".into())),
        Box::new(PipelineExpr::Single("never-run".into())),
    );

    let output = executor.execute(&expr, None).await.unwrap();
    assert_eq!(output.status, PipelineStatus::Failed);
    assert_eq!(output.workflow_id, "fail-w");
}

#[tokio::test]
async fn test_json_output_populates_output_data() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    store
        .save(&make_workflow(
            "json-w",
            vec![shell_step(1, "JSON output", r#"echo '{"key":"value"}'"#)],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Single("json-w".into());
    let output = executor.execute(&expr, None).await.unwrap();

    assert_eq!(output.status, PipelineStatus::Success);
    assert!(output.output_data.is_some());
    let data = output.output_data.unwrap();
    assert_eq!(data["key"], "value");
}

#[tokio::test]
async fn test_no_input_replaces_with_empty() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    store
        .save(&make_workflow(
            "no-input",
            vec![shell_step(1, "Echo", "echo before-{{input}}-after")],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Single("no-input".into());
    let output = executor.execute(&expr, None).await.unwrap();

    // shell_escape produces '' for empty input; shell evaluates '' as empty string
    assert_eq!(output.output_text.as_deref(), Some("before--after"));
}

#[tokio::test]
async fn test_triple_pipe_chain() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    store
        .save(&make_workflow(
            "p1",
            vec![shell_step(1, "Start", "echo start")],
        ))
        .unwrap();
    store
        .save(&make_workflow(
            "p2",
            vec![shell_step(1, "Middle", "echo mid-{{input}}")],
        ))
        .unwrap();
    store
        .save(&make_workflow(
            "p3",
            vec![shell_step(1, "End", "echo end-{{input}}")],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    // p1 | p2 | p3
    let expr = PipelineExpr::Pipe(
        Box::new(PipelineExpr::Pipe(
            Box::new(PipelineExpr::Single("p1".into())),
            Box::new(PipelineExpr::Single("p2".into())),
        )),
        Box::new(PipelineExpr::Single("p3".into())),
    );

    let output = executor.execute(&expr, None).await.unwrap();
    assert_eq!(output.status, PipelineStatus::Success);
    // p1→"start", p2→"mid-start", p3→"end-mid-start"
    assert_eq!(output.output_text.as_deref(), Some("end-mid-start"));
}

// ─── Phase 3: Parallel execution tests ──────────────────────────

#[tokio::test]
async fn test_parallel_two_workflows() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    store
        .save(&make_workflow(
            "par-a",
            vec![shell_step(1, "A", "echo alpha")],
        ))
        .unwrap();
    store
        .save(&make_workflow(
            "par-b",
            vec![shell_step(1, "B", "echo beta")],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Parallel(vec![
        PipelineExpr::Single("par-a".into()),
        PipelineExpr::Single("par-b".into()),
    ]);

    let output = executor.execute(&expr, None).await.unwrap();
    assert_eq!(output.status, PipelineStatus::Success);
    assert_eq!(output.exit_code, 0);
    // Both outputs present, separated by ---
    let text = output.output_text.unwrap();
    assert!(text.contains("alpha"), "expected alpha in: {}", text);
    assert!(text.contains("beta"), "expected beta in: {}", text);
    assert!(text.contains("---"), "expected separator in: {}", text);
}

#[tokio::test]
async fn test_parallel_actually_concurrent() {
    // Rendezvous, not a stopwatch: each branch drops its own marker file,
    // then waits for the *other* branch's marker. Both can only finish if
    // they are alive at the same time; run back to back, the first branch
    // waits for a marker that never appears and exits 1.
    //
    // A wall-clock comparison (the previous version) flaked on Windows
    // CI: `sh` spawn overhead there dwarfs any sleep we can afford, e.g.
    // "parallel 1.24s vs sequential 1.39s" against a 0.8 ratio bound.
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);
    let sync = TempDir::new().unwrap();
    // Forward slashes so Git-for-Windows `sh` reads the path verbatim.
    let dir = sync.path().display().to_string().replace('\\', "/");

    // 50 polls × 0.2s ≥ 10s of patience (more in practice: every pass
    // also pays a `sleep` spawn). Only exhausted when the branches are
    // NOT concurrent, so it costs nothing on the passing path, and stays
    // well inside nextest's 60s slow-timeout on the failing one.
    let rendezvous = |me: &str, peer: &str| {
        format!(
            "touch '{dir}/{me}' || exit 1; i=0; \
                 while [ ! -e '{dir}/{peer}' ]; do \
                 i=$((i+1)); [ \"$i\" -ge 50 ] && exit 1; sleep 0.2; \
                 done; echo {me}-done"
        )
    };

    store
        .save(&make_workflow(
            "meet-a",
            vec![shell_step(1, "Meet A", &rendezvous("a", "b"))],
        ))
        .unwrap();
    store
        .save(&make_workflow(
            "meet-b",
            vec![shell_step(1, "Meet B", &rendezvous("b", "a"))],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Parallel(vec![
        PipelineExpr::Single("meet-a".into()),
        PipelineExpr::Single("meet-b".into()),
    ]);

    let output = executor.execute(&expr, None).await.unwrap();
    let text = output.output_text.clone().unwrap_or_default();
    assert_eq!(
        output.status,
        PipelineStatus::Success,
        "parallel branches never overlapped (one gave up waiting for the other): {text}"
    );
    assert!(text.contains("a-done"), "expected a-done in: {text}");
    assert!(text.contains("b-done"), "expected b-done in: {text}");
}

#[tokio::test]
async fn test_parallel_output_merging() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    store
        .save(&make_workflow(
            "json-a",
            vec![shell_step(1, "JSON A", r#"echo '{"a":1}'"#)],
        ))
        .unwrap();
    store
        .save(&make_workflow(
            "json-b",
            vec![shell_step(1, "JSON B", r#"echo '{"b":2}'"#)],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Parallel(vec![
        PipelineExpr::Single("json-a".into()),
        PipelineExpr::Single("json-b".into()),
    ]);

    let output = executor.execute(&expr, None).await.unwrap();
    assert_eq!(output.status, PipelineStatus::Success);
    // Multiple JSON outputs should be merged into an array
    let data = output.output_data.unwrap();
    assert!(data.is_array(), "expected JSON array, got: {}", data);
    assert_eq!(data.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_parallel_exit_code_first_nonzero() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    store
        .save(&make_workflow("ok-w", vec![shell_step(1, "OK", "echo ok")]))
        .unwrap();
    store
        .save(&make_workflow(
            "fail-w2",
            vec![shell_step(1, "Fail", "exit 42")],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Parallel(vec![
        PipelineExpr::Single("ok-w".into()),
        PipelineExpr::Single("fail-w2".into()),
    ]);

    let output = executor.execute(&expr, None).await.unwrap();
    assert_eq!(output.status, PipelineStatus::Failed);
    assert_ne!(output.exit_code, 0);
}

#[tokio::test]
async fn test_fail_fast_cancellation() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    // Branch A fails immediately
    store
        .save(&make_workflow(
            "fast-fail",
            vec![shell_step(1, "Fail fast", "exit 1")],
        ))
        .unwrap();
    // Branch B sleeps for 2s — should be cancelled
    store
        .save(&make_workflow(
            "slow-cancel",
            vec![shell_step(1, "Slow", "sleep 2 && echo should-not-appear")],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store).with_fail_fast(true);
    let expr = PipelineExpr::Parallel(vec![
        PipelineExpr::Single("fast-fail".into()),
        PipelineExpr::Single("slow-cancel".into()),
    ]);

    let start = Instant::now();
    let output = executor.execute(&expr, None).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(output.status, PipelineStatus::Failed);
    // Should complete quickly (not wait 2s for the slow branch)
    // CI runners can be slow; use generous timeout
    assert!(
        elapsed.as_secs_f64() < 5.0,
        "fail-fast should have cancelled slow branch, took {:.1}s",
        elapsed.as_secs_f64()
    );
}

#[tokio::test]
async fn test_mixed_pipe_then_parallel() {
    // "w1 | w2, w3" → w2 gets w1's output via pipe, w3 runs independently
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    store
        .save(&make_workflow(
            "src",
            vec![shell_step(1, "Source", "echo source-data")],
        ))
        .unwrap();
    store
        .save(&make_workflow(
            "transform",
            vec![shell_step(1, "Transform", "echo transformed-{{input}}")],
        ))
        .unwrap();
    store
        .save(&make_workflow(
            "independent",
            vec![shell_step(1, "Indie", "echo indie-result")],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    // Parse "src | transform, independent"
    let expr = PipelineExpr::Parallel(vec![
        PipelineExpr::Pipe(
            Box::new(PipelineExpr::Single("src".into())),
            Box::new(PipelineExpr::Single("transform".into())),
        ),
        PipelineExpr::Single("independent".into()),
    ]);

    let output = executor.execute(&expr, None).await.unwrap();
    assert_eq!(output.status, PipelineStatus::Success);
    let text = output.output_text.unwrap();
    assert!(
        text.contains("transformed-source-data"),
        "pipe chain should work in parallel branch: {}",
        text
    );
    assert!(
        text.contains("indie-result"),
        "independent branch should produce output: {}",
        text
    );
}

#[tokio::test]
async fn test_parallel_duration_is_wall_clock() {
    // No absolute bound: the previous `< 1000ms` check flaked on Windows
    // CI (1543ms) because every `sh` spawn there is slow, even though the
    // branches did overlap.
    //
    // Instead: each branch meets the other (same rendezvous as
    // `test_parallel_actually_concurrent`), then both sleep 0.3s while
    // alive together. For two intervals, sum = union + overlap, so a
    // summed duration is at least 300ms *longer* than the real wall
    // clock, however slow the spawns are. A wall-clock duration can
    // never exceed what we measure around `execute`.
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);
    let sync = TempDir::new().unwrap();
    // Forward slashes so Git-for-Windows `sh` reads the path verbatim.
    let dir = sync.path().display().to_string().replace('\\', "/");

    let rendezvous_then_sleep = |me: &str, peer: &str| {
        format!(
            "touch '{dir}/{me}' || exit 1; i=0; \
                 while [ ! -e '{dir}/{peer}' ]; do \
                 i=$((i+1)); [ \"$i\" -ge 50 ] && exit 1; sleep 0.2; \
                 done; sleep 0.3; echo {me}-done"
        )
    };

    store
        .save(&make_workflow(
            "dur-a",
            vec![shell_step(1, "A", &rendezvous_then_sleep("a", "b"))],
        ))
        .unwrap();
    store
        .save(&make_workflow(
            "dur-b",
            vec![shell_step(1, "B", &rendezvous_then_sleep("b", "a"))],
        ))
        .unwrap();

    let executor = PipelineExecutor::new(store);
    let expr = PipelineExpr::Parallel(vec![
        PipelineExpr::Single("dur-a".into()),
        PipelineExpr::Single("dur-b".into()),
    ]);

    let started = Instant::now();
    let output = executor.execute(&expr, None).await.unwrap();
    let observed_ms = started.elapsed().as_millis() as u64;

    let text = output.output_text.clone().unwrap_or_default();
    assert_eq!(
        output.status,
        PipelineStatus::Success,
        "parallel branches never overlapped (one gave up waiting for the other): {text}"
    );
    assert!(
        output.duration_ms <= observed_ms,
        "duration should be wall clock (<= {observed_ms}ms observed around execute), \
             got {}ms — looks like branch durations were summed",
        output.duration_ms
    );
    assert!(
        output.duration_ms >= 300,
        "duration should cover the 0.3s both branches spent together, got {}ms",
        output.duration_ms
    );
}
