//! `hitl_key_tests`, moved out of `cli/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented one level, nothing else.

use super::super::*;

use crossterm::event::{KeyEvent, KeyEventState};

fn gate(app: &mut App) {
    app.hitl = Some(stream::HitlRequest {
        hitl_id: "h1".into(),
        step_id: None,
        tool_name: "write_file".into(),
        tool_input: serde_json::json!({}),
        prompt: "approve?".into(),
        created_at: std::time::Instant::now(),
    });
}

fn key(c: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

async fn type_str(app: &mut App, s: &str, tx: &mpsc::Sender<StreamMsg>) {
    for c in s.chars() {
        handle_event(app, key(c), tx).await;
    }
}

/// A non-character key (arrows, Enter, Esc) — the menu's live keys.
fn code(c: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code: c,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

/// #939 §3: the composer guard above is right, but it made the composer a
/// trap — every decision key became text and `Ctrl+U`, the documented way
/// to empty the line, fell through to the textarea and deleted a single
/// character instead. With no live key that approves, the only remaining
/// exit was the 5-minute auto-deny.
#[tokio::test]
async fn ctrl_u_clears_the_composer_while_a_gate_is_open() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    gate(&mut app);

    type_str(&mut app, "why not", &tx).await;
    assert_eq!(app.input_text(), "why not");

    handle_event(&mut app, ctrl('u'), &tx).await;
    assert_eq!(
        app.input_text(),
        "",
        "Ctrl+U must clear the whole line, not delete one character"
    );
    assert!(app.hitl.is_some(), "clearing the composer must not decide");

    // ...and with the composer empty again the decision keys are live.
    handle_event(&mut app, key('y'), &tx).await;
    assert!(app.hitl.is_none(), "the gate is answerable again");
}

/// #940: after denying a call the transcript advised restarting the agent
/// "for the step view" of a tool that never executed. The hint's premise is
/// "a tool ran but streamed no step detail" — a deny satisfies the second
/// half for free, so the flag now moves at the decision, not at the request.
/// Opens the gate the way the runtime does — through `handle_stream`, which
/// is where the flag used to be set. Calling `gate()` directly would skip
/// the very line under test and pass either way.
fn gate_via_stream(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    // An ask-first session: with the default auto-approve the request
    // would be answered on arrival and no gate would open to test.
    app.auto_approve = false;
    let task_id = "t1".to_string();
    app.current_task_id = Some(task_id.clone());
    let req = stream::HitlRequest {
        hitl_id: "h1".into(),
        step_id: None,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({}),
        prompt: "approve?".into(),
        created_at: std::time::Instant::now(),
    };
    handle_stream(app, StreamMsg::Hitl { req, task_id }, tx);
    assert!(app.hitl.is_some(), "the gate opened");
}

/// An image staged while a turn is generating must not be split from its
/// caption: `turn/steer` is text-only, so the steer would go out without
/// the image and the composer would be cleared under it.
#[tokio::test]
async fn image_does_not_ride_a_steer_and_stays_staged() {
    let (tx, mut rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    app.begin_user_turn("working");
    app.pending_image = Some(("image/png".into(), "AAAA".into()));
    app.set_input("do you see it?");
    submit(&mut app, &tx).await;
    assert!(app.pending_image.is_some(), "image still staged");
    assert_eq!(app.input_text(), "do you see it?", "caption kept");
    assert!(
        !app.messages.iter().any(|m| m.text.contains("steering")),
        "no steer was announced"
    );
    assert!(app.messages.iter().any(|m| m.text.contains("image")));
    assert!(rx.try_recv().is_err(), "nothing was dialled");
}

#[tokio::test]
async fn denying_a_call_does_not_arm_the_old_runtime_hint() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    app.begin_user_turn("do it");
    gate_via_stream(&mut app, &tx);
    handle_event(&mut app, key('n'), &tx).await;
    assert!(app.hitl.is_none(), "the deny landed");
    assert!(
        !app.saw_hitl_this_turn,
        "a denied call never ran, so nothing is missing a step stream"
    );
    app.maybe_step_hint();
    assert!(
        !app.messages.iter().any(|m| m.text.contains("restart")),
        "no restart advice for a tool that did not execute"
    );
}

/// Control for the above: an *approved* call with no step events is exactly
/// the old-runtime case the hint exists for, and must still fire.
#[tokio::test]
async fn approving_a_call_still_arms_the_old_runtime_hint() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    app.begin_user_turn("do it");
    gate_via_stream(&mut app, &tx);
    handle_event(&mut app, key('y'), &tx).await;
    assert!(app.saw_hitl_this_turn, "an approved call executes");
    app.maybe_step_hint();
    assert!(
        app.messages.iter().any(|m| m.text.contains("restart")),
        "the old-runtime hint must survive the #940 fix"
    );
}

/// The bug this guards: typing a message while a gate was open let the
/// first `a` approve the call AND grant the tool for the whole session,
/// with the character deleted from the message ("modal" arrived "modl").
#[tokio::test]
async fn typing_a_message_never_decides_the_gate() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    // Ask-first session (`--ask`): the gate is the operator's to decide.
    app.auto_approve = false;
    gate(&mut app);

    type_str(&mut app, "add the test", &tx).await;

    assert!(app.hitl.is_some(), "gate must still be pending");
    assert!(app.session_tool_allow.is_empty(), "no session grant");
    assert!(!app.auto_approve, "no blanket auto-approve");
    assert_eq!(app.input_text(), "add the test", "text must be intact");
}

/// Once the composer holds text the operator is writing, not deciding —
/// even the single-press keys are ordinary characters.
#[tokio::test]
async fn decision_keys_are_text_once_the_composer_is_dirty() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    gate(&mut app);

    type_str(&mut app, "why", &tx).await; // 'y' must not approve
    type_str(&mut app, "not", &tx).await; // 'n' must not deny

    assert!(app.hitl.is_some(), "gate must still be pending");
    assert_eq!(app.input_text(), "whynot");
}

/// Deliberate path: arrow to the per-tool row, press Enter. One keystroke
/// can never hand out a session grant — the cursor starts on `Yes`.
#[tokio::test]
async fn menu_grants_the_session_allow_on_enter() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    gate(&mut app);
    app.hitl_selected = 0;

    handle_event(&mut app, code(KeyCode::Down), &tx).await;
    assert_eq!(app.hitl_selected, 1, "row 2 is the per-tool grant");
    assert!(app.hitl.is_some(), "moving the cursor decides nothing");
    assert!(app.session_tool_allow.is_empty(), "no grant until Enter");

    handle_event(&mut app, code(KeyCode::Enter), &tx).await;
    assert!(app.session_tool_allow.contains("write_file"));
    assert!(app.hitl.is_none(), "Enter resolves the gate");
    assert_eq!(app.input_text(), "", "no stray character");
}

/// The digit shortcut takes the same path as arrow+Enter, and `4` is the
/// deny row rather than a grant.
#[tokio::test]
async fn digit_shortcuts_pick_rows_directly() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    gate(&mut app);

    handle_event(&mut app, key('3'), &tx).await;
    assert!(app.auto_approve, "row 3 grants every tool");
    assert!(app.hitl.is_none());
    assert_eq!(app.input_text(), "", "the digit must not land as text");

    let mut app = App::test_fixture();
    app.auto_approve = false;
    gate(&mut app);
    handle_event(&mut app, key('4'), &tx).await;
    assert!(app.hitl.is_none(), "row 4 denies");
    assert!(app.session_tool_allow.is_empty());
    assert!(!app.auto_approve);
}

/// The menu's reason for existing: ↑/↓ and Enter cannot collide with typed
/// text, so a denial can carry the message the operator is writing, while
/// the digits step aside and stay ordinary characters.
#[tokio::test]
async fn menu_keys_stay_live_while_the_composer_has_text() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    app.auto_approve = false;
    gate(&mut app);

    type_str(&mut app, "use 1 worker", &tx).await;
    assert!(app.hitl.is_some(), "digits must not decide");
    assert!(app.session_tool_allow.is_empty());
    assert_eq!(app.input_text(), "use 1 worker", "text intact");

    handle_event(&mut app, code(KeyCode::Down), &tx).await;
    handle_event(&mut app, code(KeyCode::Down), &tx).await;
    handle_event(&mut app, code(KeyCode::Down), &tx).await;
    assert_eq!(app.hitl_selected, 3, "cursor still moves");
    handle_event(&mut app, code(KeyCode::Enter), &tx).await;
    assert!(app.hitl.is_none(), "Enter still decides");
}

/// `/auto off` used to clear only the blanket flag, so tools muted with
/// `[a]` kept skipping the gate while the CLI announced "tool calls ask
/// again". Nothing else ever cleared them: a grant was permanent for the
/// session, including one pressed by accident.
#[tokio::test]
async fn auto_off_revokes_per_tool_session_grants() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    app.auto_approve = true;
    app.session_tool_allow.insert("write_file".into());
    app.session_tool_allow.insert("bash".into());

    handle_slash(&mut app, SlashCmd::Auto(Some(false)), &tx).await;

    assert!(!app.auto_approve);
    assert!(
        app.session_tool_allow.is_empty(),
        "per-tool grants must be revoked too"
    );
    let said = app
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::System)
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        said.contains("bash") && said.contains("write_file"),
        "{said}"
    );
}

/// A gate nobody answers is denied by the runtime on its own deadline and
/// the CLI is never told. The request used to sit in `app.hitl` forever,
/// so the status line kept asking for a decision on a dead request while
/// the transcript already said it had been denied.
#[test]
fn stale_gate_is_retired_so_the_status_line_stops_asking() {
    let mut app = App::test_fixture();
    gate(&mut app);

    // Fresh: nothing to retire.
    assert!(!expire_stale_hitl(&mut app));
    assert!(app.hitl.is_some());

    // Past the gate's deadline: retired, and the operator is told why.
    if let Some(req) = &mut app.hitl {
        req.created_at = std::time::Instant::now()
            - crate::hitl::gate::DEFAULT_TIMEOUT
            - std::time::Duration::from_secs(1);
    }
    assert!(expire_stale_hitl(&mut app));
    assert!(app.hitl.is_none(), "status line must stop asking");
    let said = app
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::System)
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        said.contains("write_file") && said.contains("timed out"),
        "{said}"
    );

    // Idempotent: nothing left to retire.
    assert!(!expire_stale_hitl(&mut app));
}
