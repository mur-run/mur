use super::{StepEvent, parse_step, parse_step_tokens};

#[test]
fn step_tokens_parses_and_a_frame_without_a_count_is_dropped() {
    let p = serde_json::json!({ "step_id": "s1", "task_id": "t1", "tokens": 100 });
    match parse_step_tokens(&p) {
        Some(StepEvent::Tokens {
            step_id,
            task_id,
            tokens,
        }) => {
            assert_eq!(
                (step_id.as_str(), task_id.as_str(), tokens),
                ("s1", "t1", 100)
            );
        }
        other => panic!("expected Tokens, got {other:?}"),
    }
    assert!(parse_step_tokens(&serde_json::json!({ "step_id": "s1" })).is_none());
}

#[test]
fn parses_started() {
    let p = serde_json::json!({
        "step_id": "s1",
        "task_id": "t1",
        "name": "edit",
        "args": { "path": "a.rs" }
    });
    match parse_step(&p, false) {
        StepEvent::Started {
            step_id,
            task_id,
            name,
            args,
        } => {
            assert_eq!(step_id, "s1");
            assert_eq!(task_id, "t1");
            assert_eq!(name, "edit");
            assert_eq!(args["path"], "a.rs");
        }
        other => panic!("expected Started, got {other:?}"),
    }
}

#[test]
fn parses_completed() {
    let p = serde_json::json!({
        "step_id": "s2",
        "task_id": "t2",
        "ok": true,
        "output": "done",
        "truncated": false,
        "full_len": 4u64,
        "error": null,
        "duration_ms": 123u64,
        "denied": false,
        "running": false
    });
    match parse_step(&p, true) {
        StepEvent::Completed {
            step_id,
            task_id,
            ok,
            output,
            truncated,
            full_len,
            error,
            duration_ms,
            denied,
            running,
        } => {
            assert_eq!(step_id, "s2");
            assert_eq!(task_id, "t2");
            assert!(ok);
            assert_eq!(output, "done");
            assert!(!truncated);
            assert_eq!(full_len, 4);
            assert!(error.is_none());
            assert_eq!(duration_ms, 123);
            assert!(!denied);
            assert!(!running);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[test]
fn parses_completed_running_flag() {
    let p = serde_json::json!({
        "step_id": "s3", "task_id": "t3", "ok": true, "output": "…",
        "truncated": false, "full_len": 1u64, "error": null,
        "duration_ms": 30000u64, "denied": false, "running": true
    });
    match parse_step(&p, true) {
        StepEvent::Completed { running, ok, .. } => {
            assert!(running);
            assert!(ok, "a yield is not an error");
        }
        other => panic!("{other:?}"),
    }
}
