//! `shell_turn_tests`, moved out of `cli/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented one level, nothing else.

use super::super::*;

/// Idle: the block becomes the outgoing user message, the transcript keeps
/// the one Shell card and gains no User bubble.
#[tokio::test]
async fn shell_done_while_idle_starts_a_turn_without_a_user_bubble() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    let (c_tx, _c_rx) = tokio::sync::oneshot::channel();
    let gen_id = app.shell.begin(0, c_tx).expect("slot");
    app.begin_shell("ls");
    app.append_shell_output("a\nb");
    handle_stream(
        &mut app,
        StreamMsg::ShellDone {
            gen_id,
            cmd: "ls".into(),
            end: shell::ShellEnd::Exited(0),
        },
        &tx,
    );
    assert_eq!(
        app.messages
            .iter()
            .filter(|m| m.role == Role::Shell)
            .count(),
        1
    );
    assert_eq!(
        app.messages.iter().filter(|m| m.role == Role::User).count(),
        0
    );
    assert!(app.streaming, "a turn started");
    let params = app.inflight_params.clone().expect("params kept for replay");
    let text = params["message"]["parts"][0]["text"].as_str().unwrap();
    assert!(text.contains("$ ls\na\nb"), "{text}");
    assert!(
        text.starts_with("[shell command the user ran locally]"),
        "{text}"
    );
}

/// Streaming: the block steers the live turn; no second turn starts.
#[tokio::test]
async fn shell_done_while_streaming_steers_the_live_turn() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    let before = app.begin_user_turn("working");
    let (c_tx, _c_rx) = tokio::sync::oneshot::channel();
    let gen_id = app.shell.begin(0, c_tx).expect("slot");
    app.begin_shell("ls");
    app.append_shell_output("a");
    handle_stream(
        &mut app,
        StreamMsg::ShellDone {
            gen_id,
            cmd: "ls".into(),
            end: shell::ShellEnd::Exited(0),
        },
        &tx,
    );
    assert_eq!(
        app.current_task_id.as_deref(),
        Some(before.as_str()),
        "same turn"
    );
    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("↗ steering: $ ls output")),
        "{:?}",
        app.messages
            .iter()
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        app.messages
            .iter()
            .filter(|m| m.role == Role::Shell)
            .count(),
        1
    );
}

/// Test 5 — D4: cancelled outranks every other route.
#[test]
fn cancelled_shell_output_is_never_sent() {
    for streaming in [true, false] {
        for over_budget in [true, false] {
            let route = route_shell_output(true, streaming, Some("t-1"), over_budget);
            assert!(
                matches!(route, ShellRoute::Skip(w) if w.contains("cancelled")),
                "streaming={streaming} over_budget={over_budget}: {route:?}"
            );
        }
    }
    assert!(matches!(
        route_shell_output(false, false, None, false),
        ShellRoute::Start
    ));
}

/// Test 6 — D3: with a shell running, Ctrl-C ends the shell and leaves
/// the live turn alone; a second press then behaves as it always has.
#[tokio::test]
async fn ctrl_c_ends_the_shell_before_the_turn() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    let task = app.begin_user_turn("working");
    let (c_tx, mut c_rx) = tokio::sync::oneshot::channel();
    app.shell.begin(0, c_tx).expect("slot");
    app.begin_shell("sleep 60");

    handle_ctrl_c(&mut app, &tx);

    assert_eq!(c_rx.try_recv(), Ok(()), "the shell was signalled");
    assert!(!app.shell.is_running(), "slot freed");
    assert!(app.streaming, "the turn kept running");
    assert_eq!(app.current_task_id.as_deref(), Some(task.as_str()));

    handle_ctrl_c(&mut app, &tx);
    assert!(!app.streaming, "the second press cancelled the turn");
}

/// Test 17 — D11: Ctrl-C finalises the card ON THE KEYPRESS. The
/// regression: the only event that would otherwise have stamped it is
/// the `ShellDone` whose generation this very keypress retired, so the
/// card stayed `streaming` forever — unstamped, unpersisted, under a
/// footer whose ticker had also stopped.
#[tokio::test]
async fn ctrl_c_finalises_the_card_immediately() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    let (c_tx, _c_rx) = tokio::sync::oneshot::channel();
    let gen_id = app.shell.begin(4242, c_tx).expect("slot");
    app.begin_shell("sleep 60");
    app.append_shell_output("partial");

    handle_ctrl_c(&mut app, &tx);

    let card = app
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Shell)
        .expect("card");
    assert!(!card.streaming, "the spinner stopped");
    assert!(card.text.ends_with("[cancelled]"), "{}", card.text);
    assert!(!app.shell.is_running(), "no longer Ctrl-C-able");

    // The late report changes nothing further, and must not start a turn.
    let before = app.messages.len();
    handle_stream(
        &mut app,
        StreamMsg::ShellDone {
            gen_id,
            cmd: "sleep 60".into(),
            end: shell::ShellEnd::Cancelled,
        },
        &tx,
    );
    assert_eq!(app.messages.len(), before, "nothing more was written");
    assert!(!app.streaming, "no turn was started");
}

/// Test 12 — D8, one per path, each through its REAL entry point. A test
/// that called `stop_shell` directly would have passed while any of these
/// call sites was missing.
#[tokio::test]
async fn every_teardown_path_stops_the_shell_and_retires_it() {
    for (name, trigger) in [("quit", 0u8), ("clear", 1u8), ("channel switch", 2u8)] {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        let (c_tx, mut c_rx) = tokio::sync::oneshot::channel();
        // `pid: 0` stands for "no real process" — `signal_group` refuses
        // it rather than signalling this test runner's own group. The
        // real-group kill is covered in `shell.rs`.
        let gen_id = app.shell.begin(0, c_tx).expect("slot");
        app.begin_shell("sleep 60");

        match trigger {
            0 => request_quit(&mut app, &tx),
            1 => handle_slash(&mut app, SlashCmd::Clear, &tx).await,
            // The switch path: what the `app.switch_channel(&id)` call
            // site does before switching.
            _ => {
                stop_shell(&mut app, false);
                let _ = app.switch_channel("does-not-exist");
            }
        }

        assert!(!app.shell.is_running(), "{name}: shell still running");
        assert!(!app.shell.accepts(gen_id), "{name}: generation not retired");
        if trigger != 0 {
            // Quit kills the group outright; the soft paths signal the task.
            assert_eq!(c_rx.try_recv(), Ok(()), "{name}: task not signalled");
        }

        // Whatever the dying task still emits writes nothing.
        let before = app.messages.len();
        handle_stream(
            &mut app,
            StreamMsg::ShellOutput {
                gen_id,
                chunk: "late".into(),
            },
            &tx,
        );
        handle_stream(
            &mut app,
            StreamMsg::ShellDone {
                gen_id,
                cmd: "sleep 60".into(),
                end: shell::ShellEnd::Cancelled,
            },
            &tx,
        );
        assert_eq!(app.messages.len(), before, "{name}: a stale event drew");
        assert!(!app.streaming, "{name}: a stale event started a turn");
    }
}

/// Test 11 (wiring half) — D7: a second `!cmd` is refused with a note and
/// the first keeps its slot.
#[tokio::test]
async fn a_second_bang_command_is_refused() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = App::test_fixture();
    let (c_tx, mut c_rx) = tokio::sync::oneshot::channel();
    app.shell.begin(0, c_tx).expect("slot");
    app.set_input("!echo two");

    submit(&mut app, &tx).await;

    assert!(app.shell.is_running(), "the first still holds the slot");
    assert!(
        matches!(
            c_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "the first was cancelled or its handle dropped"
    );
    let last = app.messages.last().expect("a note");
    assert!(last.text.contains("already running"), "{}", last.text);
}

/// `!` lines open the shell menu through the same refresh path as `/`,
/// and accepting a directory keeps it open on that directory.
#[test]
fn bang_lines_get_the_shell_menu_and_directories_descend() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir(t.path().join("docs")).unwrap();
    std::fs::write(t.path().join("docs/a.md"), "").unwrap();
    let mut app = App::test_fixture();
    app.cwd = Some(t.path().to_path_buf());
    app.path_bins = Some(vec!["cargo".into(), "cat".into()]);

    app.set_input("!ca");
    refresh_completion(&mut app);
    let items: Vec<String> = app
        .completion
        .as_ref()
        .unwrap()
        .items
        .iter()
        .map(|c| c.display.clone())
        .collect();
    assert_eq!(items, ["cargo", "cat"]);
    assert_eq!(app.completion.as_ref().unwrap().current, None);

    app.set_input("!ls ");
    refresh_completion(&mut app);
    assert_eq!(app.completion.as_ref().unwrap().items[0].display, "docs/");
    completion_accept(&mut app);
    assert_eq!(app.input_text(), "!ls docs/");
    let inside = app
        .completion
        .as_ref()
        .expect("menu stays open on a directory");
    assert_eq!(inside.items[0].display, "a.md");

    app.set_input("/skin ");
    refresh_completion(&mut app);
    assert!(
        app.completion
            .as_ref()
            .unwrap()
            .items
            .iter()
            .any(|c| c.display == "mur"),
        "slash menu untouched"
    );
}
