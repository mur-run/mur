//! at-mention C3: murmur `@<pane agent>[#N] text` (S2) and the display of
//! a channel-conversation refusal.

use super::super::app::Severity;
use super::super::*;
use mur_channel::ChannelService;

/// A home that outlives the test body, agents `a` and `b` on disk, and a
/// signing identity for `a` (the pane's agent).
fn app_in(home: &tempfile::TempDir) -> App {
    for n in ["a", "b"] {
        let dir = home.path().join("agents").join(n);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profile.yaml"), format!("name: {n}\n")).unwrap();
    }
    crate::channel_writer::plant_identity_for(home.path(), "a");
    let session = Session::create(home.path(), "a").unwrap();
    App::new(
        home.path().to_path_buf(),
        "a".into(),
        session,
        &super::super::theme::ANSI,
    )
}

fn type_line(app: &mut App, line: &str) {
    app.set_input(line);
}

fn channel_dirs(home: &std::path::Path) -> usize {
    std::fs::read_dir(home.join("channels"))
        .map(|d| d.flatten().filter(|e| e.path().is_dir()).count())
        .unwrap_or(0)
}

#[tokio::test]
async fn murmur_at_other_agent_sends_nothing() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    type_line(&mut app, "@b#1 please look");

    submit(&mut app, &tx).await;

    assert!(app.inflight_params.is_none(), "nothing dialed");
    assert!(!app.streaming);
    assert_eq!(channel_dirs(home.path()), 0, "nothing written");
    let last = app.messages.last().expect("a notice");
    assert!(
        last.text.contains("mur agent send '@b") && last.text.contains("this pane is bound to a"),
        "{}",
        last.text
    );
}

#[tokio::test]
async fn murmur_at_own_agent_ordinal_switches_then_sends_rest() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let svc = ChannelService::open(home.path()).unwrap();
    let older = svc.create_for_agent("a").unwrap();
    let n = svc.index().ordinal_of(&older.id).unwrap().unwrap();
    let (tx, _rx) = mpsc::channel(16);
    type_line(&mut app, &format!("@A#{n} carry on"));

    submit(&mut app, &tx).await;

    let params = app.inflight_params.clone().expect("turn started");
    assert_eq!(params["context"]["channel_id"], older.id.as_str());
    assert_eq!(params["message"]["parts"][0]["text"], "carry on");
    assert_eq!(app.channel.as_ref().map(|c| c.ordinal), Some(n), "badge");
}

#[tokio::test]
async fn murmur_at_own_agent_without_ordinal_sends_rest_in_place() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    type_line(&mut app, "@a hello there");

    submit(&mut app, &tx).await;

    let params = app.inflight_params.clone().expect("turn started");
    assert_eq!(params["message"]["parts"][0]["text"], "hello there");
}

#[tokio::test]
async fn murmur_at_own_agent_unknown_ordinal_is_an_error_and_sends_nothing() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let (tx, _rx) = mpsc::channel(16);
    type_line(&mut app, "@a#42 hi");

    submit(&mut app, &tx).await;

    assert!(app.inflight_params.is_none());
    let last = app.messages.last().unwrap();
    assert_eq!(last.severity, Severity::Error);
    assert!(
        last.text.starts_with("UNKNOWN_CONVERSATION"),
        "{}",
        last.text
    );
    assert_eq!(channel_dirs(home.path()), 0);
}

#[tokio::test]
async fn murmur_at_own_agent_non_member_channel_is_not_a_participant() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app_in(&home);
    let svc = ChannelService::open(home.path()).unwrap();
    let theirs = svc.create_for_agent("b").unwrap();
    let n = svc.index().ordinal_of(&theirs.id).unwrap().unwrap();
    let (tx, _rx) = mpsc::channel(16);
    type_line(&mut app, &format!("@a#{n} hi"));

    submit(&mut app, &tx).await;

    assert!(app.inflight_params.is_none());
    assert!(app.channel.is_none(), "the pane did not switch");
    let last = app.messages.last().unwrap();
    assert!(last.text.starts_with("NOT_A_PARTICIPANT"), "{}", last.text);
}

/// A runtime refusal of a channel turn is final: it is shown as denied and
/// the dial is not replayed (the replay would hit the same refusal).
#[test]
fn a_conversation_refusal_is_denied_not_retried() {
    let mut app = App::test_fixture();
    let (tx, _rx) = mpsc::channel(16);
    app.current_task_id = Some("t1".into());
    app.streaming = true;
    app.inflight_params = Some(serde_json::json!({"message": {}}));
    let error =
        r#"agent 'a' returned error: {"code":-32025,"message":"conversation damaged: watermark"}"#;

    handle_stream(
        &mut app,
        StreamMsg::Err {
            task_id: "t1".into(),
            error: error.into(),
        },
        &tx,
    );

    assert!(!app.send_retried, "no replay of a refused channel turn");
    assert!(!app.streaming);
    assert!(
        app.messages
            .iter()
            .any(|m| m.severity == Severity::Denied && m.text.contains("conversation damaged")),
        "shown as denied"
    );
}

#[test]
fn conversation_refusals_are_recognised_by_prefix() {
    for msg in [
        "conversation damaged: chain",
        "unknown conversation: 019f",
        "not a participant: a is not in x",
        "channel too new: v9",
        "channel is not a conversation: fleet-x",
        "conversation repairing: x",
        "conversation busy: x",
    ] {
        let wrapped = format!(r#"agent 'a' returned error: {{"code":-32020,"message":"{msg}"}}"#);
        assert!(recover::is_conversation_refusal(&wrapped), "{msg}");
        assert!(recover::is_conversation_refusal(msg), "bare {msg}");
    }
    assert!(!recover::is_conversation_refusal(
        "connect: connection refused"
    ));
}

/// B3 refuses a channel turn as a failed task carrying the §7 message, so
/// the refusal usually arrives as `Done`, not as a dial `Err`.
#[test]
fn a_refused_channel_task_is_denied() {
    let mut app = App::test_fixture();
    let (tx, _rx) = mpsc::channel(16);
    app.current_task_id = Some("t1".into());
    app.streaming = true;
    let task = serde_json::json!({
        "id": "t1",
        "state": "failed",
        "error": {"code": "CONVERSATION_BUSY", "message": "conversation busy: 019f"},
    });

    handle_stream(
        &mut app,
        StreamMsg::Done {
            task_id: "t1".into(),
            task: Box::new(task),
        },
        &tx,
    );

    assert!(!app.streaming);
    assert!(
        app.messages
            .iter()
            .any(|m| m.severity == Severity::Denied && m.text.contains("conversation busy")),
        "shown as denied"
    );
}
