//! `tests`, moved out of `stream.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented by one level, nothing else.

use super::*;

#[test]
fn hitl_respond_params_always_carry_a_surface() {
    // The defect this guards: the CLI used to send only hitl_id and allow,
    // so the runtime defaulted the surface to "unknown" and the signed
    // channel could not say which surface answered.
    let home = tempfile::tempdir().unwrap();
    for (allow, surface) in [(true, "cli"), (false, "cli"), (true, "auto")] {
        let p = hitl_respond_params(home.path(), "h-1", allow, surface);
        assert_eq!(p["hitl_id"], "h-1");
        assert_eq!(p["allow"], allow);
        assert_eq!(
            p["surface"], surface,
            "surface must be sent verbatim, never dropped"
        );
        assert!(
            !p["surface"].is_null(),
            "a missing surface is recorded as \"unknown\" by the runtime"
        );
    }
}

#[test]
fn hitl_respond_params_carry_the_token_only_on_allow() {
    use mur_common::hitl::approval_token;
    let home = tempfile::tempdir().unwrap();
    let token = approval_token::load_or_create(home.path()).unwrap();
    let allow = hitl_respond_params(home.path(), "h-1", true, "cli");
    assert_eq!(allow[approval_token::PARAM], token.as_str());
    let deny = hitl_respond_params(home.path(), "h-1", false, "cli");
    assert!(deny.get(approval_token::PARAM).is_none());
}

#[test]
fn build_params_threads_context() {
    let p = build_params("hi", "t-2", Some("t-1"), None, None);
    assert_eq!(p["message"]["parts"][0]["text"], "hi");
    assert_eq!(p["task_id"], "t-2");
    assert_eq!(p["context"]["task_id"], "t-1");
}

#[test]
fn build_params_carries_cwd_as_context_on_every_turn() {
    let dir = Path::new("/Users/x/Projects/ho-me");
    let first = build_params("hi", "t-1", None, None, Some(dir));
    assert_eq!(first["context"]["cwd"], "/Users/x/Projects/ho-me");
    assert!(first["context"].get("task_id").is_none());
    let later = build_params("hi", "t-2", Some("t-1"), None, Some(dir));
    assert_eq!(later["context"]["cwd"], "/Users/x/Projects/ho-me");
    assert_eq!(later["context"]["task_id"], "t-1");
    assert!(
        !first["message"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("working directory"),
        "the path is structured context, not prose in the message"
    );
}

#[test]
fn build_params_first_turn_has_no_context() {
    let p = build_params("hi", "t-1", None, None, None);
    assert!(p.get("context").is_none());
}

#[test]
fn build_params_attaches_image_as_data_part() {
    let p = build_params(
        "what is this?",
        "t-3",
        None,
        Some(("image/png", "QkFTRTY0")),
        None,
    );
    let parts = p["message"]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2, "text part + image data part");
    assert_eq!(parts[0]["kind"], "text");
    assert_eq!(parts[1]["kind"], "data");
    assert_eq!(parts[1]["mimeType"], "image/png");
    assert_eq!(parts[1]["data"]["base64"], "QkFTRTY0");
    // A non-PNG paste threads its own mime.
    let jpg = build_params("x", "t-5", None, Some(("image/jpeg", "QQ==")), None);
    assert_eq!(jpg["message"]["parts"][1]["mimeType"], "image/jpeg");
    // No image → no data part (back-compat).
    let plain = build_params("hi", "t-4", None, None, None);
    assert_eq!(plain["message"]["parts"].as_array().unwrap().len(), 1);
}

#[test]
fn extract_reply_finds_last_agent_message() {
    let task = json!({
        "id": "task-9",
        "messages": [
            { "role": "user", "parts": [{ "kind": "text", "text": "q" }] },
            { "role": "agent", "parts": [
                { "kind": "text", "text": "Hello " },
                { "kind": "text", "text": "world" }
            ]}
        ]
    });
    let (reply, id) = extract_reply(&task);
    assert_eq!(reply, "Hello world");
    assert_eq!(id.as_deref(), Some("task-9"));
}

/// #2071: a failed turn threads only when the runtime says it stored it.
#[test]
fn remembered_task_id_requires_the_runtime_flag() {
    let failed = json!({
        "id": "task-7",
        "state": "failed",
        "error": { "message": "tool call denied: timed out" },
        "usage": { "input_tokens": 1, "remembered": true }
    });
    assert_eq!(remembered_task_id(&failed).as_deref(), Some("task-7"));
    // An older runtime: no flag, no threading.
    let old = json!({
        "id": "task-7",
        "state": "failed",
        "error": { "message": "tool call denied: timed out" },
        "usage": { "input_tokens": 1 }
    });
    assert_eq!(remembered_task_id(&old), None);
}

#[test]
fn task_outcome_failed_surfaces_error() {
    let task = json!({ "id": "t", "state": "failed", "error": { "message": "rate limited" }, "messages": [] });
    assert_eq!(task_outcome(&task).unwrap_err(), "rate limited");
}

#[test]
fn task_outcome_cancelled_without_error_message() {
    let task = json!({ "id": "t", "state": "cancelled", "messages": [] });
    assert_eq!(task_outcome(&task).unwrap_err(), "agent task cancelled");
}

#[test]
fn attached_proposal_reads_the_reply_data_part() {
    let task = json!({
        "id": "t",
        "state": "completed",
        "messages": [{ "role": "agent", "parts": [
            { "kind": "text", "text": "needs a restart" },
            { "kind": "data", "mimeType": mur_common::proposal::PROPOSAL_MIME,
              "data": { "label": "restart to apply", "kind": "restart" } }
        ]}]
    });
    let p = attached_proposal(&task).expect("chip");
    assert_eq!(p.kind, mur_common::proposal::ProposalKind::Restart);
    assert_eq!(task_outcome(&task).unwrap().0, "needs a restart");
}

#[test]
fn task_outcome_completed_returns_reply() {
    let task = json!({
        "id": "t9",
        "state": "completed",
        "messages": [{ "role": "agent", "parts": [{ "kind": "text", "text": "hi" }] }]
    });
    let (reply, id) = task_outcome(&task).unwrap();
    assert_eq!(reply, "hi");
    assert_eq!(id.as_deref(), Some("t9"));
}

#[test]
fn stream_msg_task_id_accessor() {
    let d = StreamMsg::Delta {
        task_id: "x".into(),
        text: "a".into(),
        thinking: false,
    };
    assert_eq!(d.task_id(), Some("x"));
    assert_eq!(StreamMsg::Note("n".into()).task_id(), None);
}

#[test]
fn hitl_request_parses_fields_with_fallback() {
    let h = HitlRequest::from_value(
        json!({
            "hitl_id": "h1",
            "tool_name": "bash",
            "tool_input": { "command": "ls" }
        }),
        std::time::Instant::now(),
    );
    assert_eq!(h.hitl_id, "h1");
    assert_eq!(h.tool_name, "bash");
    assert_eq!(h.tool_input["command"], "ls");
}
