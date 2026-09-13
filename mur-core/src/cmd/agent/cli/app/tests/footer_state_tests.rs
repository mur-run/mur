//! `footer_state_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;

#[test]
fn apply_usage_accumulates_session_and_sets_turn() {
    let mut a = App::test_fixture();
    a.apply_usage(
        &serde_json::json!({ "input_tokens": 100, "output_tokens": 20, "context_tokens": 100 }),
    );
    a.apply_usage(
        &serde_json::json!({ "input_tokens": 50, "output_tokens": 10, "context_tokens": 150 }),
    );
    assert_eq!(a.turn_in, 50);
    assert_eq!(a.turn_out, 10);
    assert_eq!(a.session_in, 150);
    assert_eq!(a.session_out, 30);
    assert_eq!(a.ctx_tokens, 150);
}

#[test]
fn begin_user_turn_resets_turn_counters_and_arms_clock() {
    let mut a = App::test_fixture();
    // Prime some prior-turn state.
    a.apply_usage(&serde_json::json!({ "input_tokens": 100, "output_tokens": 20 }));
    a.begin_user_turn("hi");
    assert_eq!(a.turn_in, 0, "turn_in reset");
    assert_eq!(a.turn_out, 0, "turn_out reset");
    assert!(!a.saw_step_this_turn, "saw_step reset");
    assert!(a.turn_started.is_some(), "clock armed");
    // session accumulators must NOT be cleared by begin_user_turn.
    assert_eq!(a.session_in, 100, "session_in survives begin_user_turn");
    assert_eq!(a.session_out, 20, "session_out survives begin_user_turn");
}

#[test]
fn begin_user_turn_clears_stale_pending_suggestions() {
    let mut a = App::test_fixture();
    // Simulate a prior turn that set suggestions but never revealed them
    // (e.g., the turn ended in Err or was cancelled via Ctrl+C).
    a.pending_suggestions = vec![super::super::super::suggest::Suggestion {
        text: "stale suggestion".to_string(),
        desc: None,
    }];
    a.begin_user_turn("new turn");
    assert!(
        a.pending_suggestions.is_empty(),
        "pending_suggestions must be cleared at the start of each turn"
    );
}

#[test]
fn finish_agent_turn_clears_clock() {
    let mut a = App::test_fixture();
    a.begin_user_turn("hi");
    assert!(a.turn_started.is_some());
    a.finish_agent_turn("ok".into(), None);
    assert!(a.turn_started.is_none(), "clock cleared after finish");
}

#[test]
fn finish_partial_clears_clock() {
    let mut a = App::test_fixture();
    a.begin_user_turn("hi");
    assert!(a.turn_started.is_some());
    a.finish_partial();
    assert!(a.turn_started.is_none());
}

#[test]
fn context_tokens_update_on_apply() {
    let mut a = App::test_fixture();
    a.apply_usage(&serde_json::json!({ "input_tokens": 10, "output_tokens": 5 }));
    assert_eq!(a.ctx_tokens, 0, "no context_tokens field → unchanged");
    a.apply_usage(
        &serde_json::json!({ "input_tokens": 10, "output_tokens": 5, "context_tokens": 42000 }),
    );
    assert_eq!(a.ctx_tokens, 42000);
}

#[test]
fn old_runtime_hitl_without_steps_shows_hint_once() {
    let mut a = App::test_fixture();
    a.begin_user_turn("do it");
    a.saw_hitl_this_turn = true; // hitl arrived, no step events => old runtime
    a.maybe_step_hint();
    assert!(a.step_hint_shown);
    let n = a
        .messages
        .iter()
        .filter(|m| m.role == Role::System && m.text.contains("restart"))
        .count();
    assert_eq!(n, 1);
    // second such turn: not shown again
    a.begin_user_turn("again");
    a.saw_hitl_this_turn = true;
    a.maybe_step_hint();
    let n2 = a
        .messages
        .iter()
        .filter(|m| m.role == Role::System && m.text.contains("restart"))
        .count();
    assert_eq!(n2, 1);
}

#[test]
fn new_runtime_with_steps_shows_no_hint() {
    let mut a = App::test_fixture();
    a.begin_user_turn("do it");
    a.saw_hitl_this_turn = true;
    a.saw_step_this_turn = true; // new runtime emitted step events
    a.maybe_step_hint();
    assert!(!a.step_hint_shown);
    assert!(!a.messages.iter().any(|m| m.text.contains("restart")));
}
