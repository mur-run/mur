//! `awaiting_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;

#[test]
fn mark_and_clear_awaiting_by_step_id() {
    let mut a = App::test_fixture();
    a.begin_user_turn("edit it");
    a.push_step_started(
        "s1".into(),
        "edit".into(),
        serde_json::json!({"file_path":"a.rs"}),
    );
    a.update_step_completed("s1", true, "ok".into(), false, 2, None, 5, false, false);
    a.mark_card_awaiting("s1");
    let card = a.messages.iter().find_map(|m| m.step.as_ref()).unwrap();
    assert!(card.awaiting_hitl);
    a.clear_card_awaiting("s1");
    let card = a.messages.iter().find_map(|m| m.step.as_ref()).unwrap();
    assert!(!card.awaiting_hitl);
}

/// A card can be flushed to frozen scrollback while its gate is still
/// open. The marker flag stays set (a full redraw and the Ctrl+O dump read
/// it); the decision itself never depended on the card being on screen —
/// the docked panel is always there.
#[test]
fn a_flushed_card_keeps_its_awaiting_marker() {
    let mut a = App::test_fixture();
    a.begin_user_turn("edit it");
    a.push_step_started(
        "s1".into(),
        "edit".into(),
        serde_json::json!({"file_path":"a.rs"}),
    );
    a.update_step_completed("s1", true, "ok".into(), false, 2, None, 5, false, false);
    a.mark_card_awaiting("s1");
    a.flushed_upto = a.messages.len();
    let card = a
        .messages
        .iter()
        .find_map(|m| m.step.as_ref().filter(|c| c.id == "s1"))
        .unwrap();
    assert!(card.awaiting_hitl);
}
