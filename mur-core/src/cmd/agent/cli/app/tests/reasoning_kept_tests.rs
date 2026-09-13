//! `reasoning_kept_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;

#[test]
fn thinking_survives_turn_finish() {
    let mut a = App::test_fixture();
    a.begin_user_turn("hi");
    a.append_delta("let me think", true); // thinking delta
    a.append_delta("the answer", false);
    a.finish_agent_turn("the answer".into(), Some("t1".into()));
    let last = a.messages.last().unwrap();
    assert_eq!(last.role, Role::Agent);
    assert_eq!(last.thinking, "let me think"); // not cleared
    assert!(!last.streaming);
}
