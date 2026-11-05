//! Spec §4 "Stacking" and §5 role label: consecutive step cards share no gap
//! row, a turn between them restores one on each side, and the measured block
//! equals the painted one.

use super::message_block;
use crate::cmd::agent::cli::app::{App, ChatMsg, Role};
use crate::cmd::agent::cli::step::{CallOutcome, StepCard};

fn card(id: &str) -> ChatMsg {
    let mut c = StepCard::new(
        id.into(),
        "bash".into(),
        serde_json::json!({ "command": "ls" }),
    );
    c.complete(CallOutcome::Ok, "a\nb".into(), false, 2, None, 15);
    ChatMsg::tool_for_test(c)
}

fn text_of(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

#[test]
fn consecutive_cards_stack() {
    let mut app = App::test_fixture();
    app.messages = vec![
        card("1"),
        card("2"),
        ChatMsg::for_test(Role::Agent, "done"),
        card("3"),
    ];
    let blank = |rows: &[String]| rows.first().is_some_and(|r| r.trim().is_empty());
    let block =
        |i: usize, measured| text_of(&message_block(&app, i, &app.messages[i], 0, measured));

    assert!(
        !blank(&block(1, false)),
        "no gap between two cards: {:?}",
        block(1, false)
    );
    assert!(blank(&block(2, false)), "gap before the agent turn");
    assert!(blank(&block(3, false)), "gap after the agent turn");
    for i in 0..app.messages.len() {
        assert_eq!(
            block(i, true).len(),
            block(i, false).len(),
            "measured == painted at {i}"
        );
    }
}

#[test]
fn role_label_is_the_display_name() {
    let mut app = App::test_fixture();
    app.agent_label = "MUR".into();
    app.messages = vec![ChatMsg::for_test(Role::Agent, "hello")];
    let rows = text_of(&message_block(&app, 0, &app.messages[0], 0, false));
    assert_eq!(rows[0], "● MUR", "{rows:?}");
}
