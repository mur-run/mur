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

#[test]
fn inline_row_visible_only_for_a_card_in_the_live_band() {
    let mut a = App::test_fixture();
    a.begin_user_turn("edit it");
    a.push_step_started(
        "s1".into(),
        "edit".into(),
        serde_json::json!({"file_path":"a.rs"}),
    );
    a.update_step_completed("s1", true, "ok".into(), false, 2, None, 5, false, false);

    // No card exists for this step_id: the renderer must be told so it can
    // fall back to the modal instead of assuming inline approval worked.
    a.mark_card_awaiting("no-such-step");
    assert!(!a.hitl_inline_visible(Some("no-such-step")));

    // A card exists for "s1" and is still in the live band: the inline row
    // is genuinely on screen, so the modal stands down.
    a.mark_card_awaiting("s1");
    assert!(a.hitl_inline_visible(Some("s1")));

    // A gate with no step_id at all can never be inline.
    assert!(!a.hitl_inline_visible(None));
}

/// A card already committed to the terminal's native scrollback can never
/// repaint, so the inline approval row on it shows the operator nothing.
/// Claiming it is visible suppressed the modal too and left an approval
/// request with no surface at all — no modal, no row, no tool name.
#[test]
fn flushed_card_is_not_visible_so_the_modal_takes_over() {
    let mut a = App::test_fixture();
    a.begin_user_turn("edit it");
    a.push_step_started(
        "s1".into(),
        "edit".into(),
        serde_json::json!({"file_path":"a.rs"}),
    );
    a.update_step_completed("s1", true, "ok".into(), false, 2, None, 5, false, false);
    a.mark_card_awaiting("s1");
    assert!(a.hitl_inline_visible(Some("s1")), "live band: row shows");

    // The band overflows and the card is committed to scrollback — this
    // can happen while the gate is still open, which is why visibility is
    // recomputed per frame instead of cached at attach time.
    a.flushed_upto = a.messages.len();
    assert!(
        !a.hitl_inline_visible(Some("s1")),
        "frozen rows cannot show an approval request"
    );

    // The flag itself stays set: a full redraw / transcript dump uses it.
    let card = a
        .messages
        .iter()
        .find_map(|m| m.step.as_ref().filter(|c| c.id == "s1"))
        .unwrap();
    assert!(card.awaiting_hitl);
}
