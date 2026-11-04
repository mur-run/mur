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

/// #2260: options arriving while the user has typed ahead are neither shown
/// as a chooser nor dropped — they go to the transcript as text and stay
/// pending; once the composer empties they surface as the chooser.
#[test]
fn reveal_with_typed_ahead_composer_defers_then_shows_when_empty() {
    use super::super::super::suggest::Suggestion;
    let mut a = App::test_fixture();
    a.begin_user_turn("hi");
    a.input.insert_str("next question");
    a.pending_suggestions = vec![
        Suggestion {
            text: "yes".into(),
            desc: None,
        },
        Suggestion {
            text: "no".into(),
            desc: Some("skip it".into()),
        },
    ];
    a.finish_agent_turn("ok".into(), None);
    a.reveal_suggestions();
    assert!(
        a.completion.is_none(),
        "no chooser over a non-empty composer"
    );
    assert_eq!(a.pending_suggestions.len(), 2, "options stay pending");
    let fallback = a
        .messages
        .iter()
        .filter(|m| m.role == Role::System && m.text.contains("Suggested replies"))
        .count();
    assert_eq!(fallback, 1, "options listed once in the transcript");
    assert!(a.messages.last().unwrap().text.contains("`no` — skip it"));

    // Still non-empty: a second reveal neither repeats the line nor opens.
    a.reveal_deferred_suggestions();
    assert!(a.completion.is_none());
    assert_eq!(
        a.messages
            .iter()
            .filter(|m| m.role == Role::System && m.text.contains("Suggested replies"))
            .count(),
        1
    );

    // Composer emptied → chooser opens, pending drained.
    a.clear_input();
    a.reveal_deferred_suggestions();
    assert!(
        a.completion
            .as_ref()
            .is_some_and(|c| c.spaced && c.items.len() == 2)
    );
    assert!(a.pending_suggestions.is_empty());
}

/// #2260: deferred options are not surfaced while a new turn is streaming.
#[test]
fn deferred_suggestions_wait_while_streaming() {
    use super::super::super::suggest::Suggestion;
    let mut a = App::test_fixture();
    a.pending_suggestions = vec![Suggestion {
        text: "yes".into(),
        desc: None,
    }];
    a.streaming = true;
    a.reveal_deferred_suggestions();
    assert!(a.proposal.is_none());
    assert_eq!(a.pending_suggestions.len(), 1);
}
