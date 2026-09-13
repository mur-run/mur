//! `state`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;
use tempfile::tempdir;

fn app() -> App {
    let home = tempdir().unwrap();
    let session = Session::create(home.path(), "a").unwrap();
    App::new(
        home.path().to_path_buf(),
        "a".into(),
        session,
        &super::super::super::theme::ANSI,
    )
}

#[test]
fn input_debounce_arms_and_fires_once() {
    use std::time::{Duration, Instant};
    let mut app = app();
    let t0 = Instant::now();

    // No edit → nothing armed, nothing due.
    arm_input_debounce(&mut app, t0);
    assert!(take_due_input(&mut app, t0 + Duration::from_secs(1)).is_none());

    // Edit arms the deadline; before expiry nothing fires.
    app.set_input("run boo");
    arm_input_debounce(&mut app, t0);
    assert!(take_due_input(&mut app, t0).is_none());

    // Continued typing re-arms (debounce reset).
    app.set_input("run book");
    arm_input_debounce(&mut app, t0 + Duration::from_millis(100));

    // After the (re-armed) deadline the latest snapshot fires exactly once.
    let due = t0 + Duration::from_millis(100 + mur_common::panel::INPUT_DEBOUNCE_MS + 1);
    assert_eq!(take_due_input(&mut app, due).as_deref(), Some("run book"));
    assert!(take_due_input(&mut app, due).is_none()); // no repeat

    // Unchanged text never re-fires even after another arm pass.
    arm_input_debounce(&mut app, due);
    assert!(take_due_input(&mut app, due + Duration::from_secs(1)).is_none());

    // Clearing the input fires an empty snapshot (panel resets to cwd mode).
    app.clear_input();
    arm_input_debounce(&mut app, due);
    let later = due + Duration::from_millis(mur_common::panel::INPUT_DEBOUNCE_MS + 1);
    assert_eq!(take_due_input(&mut app, later).as_deref(), Some(""));
}

/// Helper that borrows an existing TempDir so the directory survives the test.
fn app_at(home: &tempfile::TempDir) -> App {
    let session = Session::create(home.path(), "a").unwrap();
    App::new(
        home.path().to_path_buf(),
        "a".into(),
        session,
        &super::super::super::theme::ANSI,
    )
}

#[test]
fn parse_slash_variants() {
    assert_eq!(parse_slash("/help"), Some(SlashCmd::Help));
    assert_eq!(parse_slash("/new"), Some(SlashCmd::Clear));
    assert_eq!(parse_slash("/card"), Some(SlashCmd::Card));
    assert_eq!(parse_slash("/memories"), Some(SlashCmd::Memories));
    assert_eq!(
        parse_slash("/forget last"),
        Some(SlashCmd::Forget(Some("last".into())))
    );
    assert_eq!(
        parse_slash("/remember reply in zh-TW"),
        Some(SlashCmd::Remember(vec![
            "reply".into(),
            "in".into(),
            "zh-TW".into()
        ]))
    );
    assert_eq!(parse_slash("/q"), Some(SlashCmd::Quit));
    assert_eq!(
        parse_slash("/bogus"),
        Some(SlashCmd::Unknown("bogus".into()))
    );
    assert_eq!(parse_slash("hello"), None);
    assert_eq!(parse_slash("  not/a/cmd"), None);
}

#[test]
fn parses_panel() {
    assert_eq!(parse_slash("/panel"), Some(SlashCmd::Panel(vec![])));
    assert_eq!(
        parse_slash("/panel preview out/report.html"),
        Some(SlashCmd::Panel(vec![
            "preview".to_string(),
            "out/report.html".to_string()
        ]))
    );
}

#[test]
fn parse_slash_skin_variants() {
    assert_eq!(parse_slash("/skin"), Some(SlashCmd::Skin(None)));
    assert_eq!(
        parse_slash("/skin mur"),
        Some(SlashCmd::Skin(Some("mur".into())))
    );
    assert_eq!(
        parse_slash("/theme light"),
        Some(SlashCmd::Skin(Some("light".into())))
    );
}

#[test]
fn turn_lifecycle_threads_context() {
    let mut a = app();
    let tid = a.begin_user_turn("hi");
    assert!(a.streaming);
    assert_eq!(a.current_task_id.as_deref(), Some(tid.as_str()));
    a.append_delta("Hel", false);
    a.append_delta("lo", false);
    a.append_delta("(thinking)", true);
    assert_eq!(a.messages.last().unwrap().text, "Hello");
    a.finish_agent_turn("Hello!".into(), Some("server-task-1".into()));
    assert!(!a.streaming);
    assert_eq!(a.context_task_id.as_deref(), Some("server-task-1"));
    assert_eq!(a.messages.last().unwrap().text, "Hello!");
    assert!(!a.messages.last().unwrap().streaming);
}

#[test]
fn late_finish_after_cancel_does_not_persist_or_thread() {
    // After Ctrl+C (finish_partial), a stale worker's Done must not rewrite
    // the bubble, persist a phantom line, or thread the cancelled task id.
    let mut a = app();
    a.begin_user_turn("hi");
    a.append_delta("partial", false);
    a.finish_partial();
    let ctx_before = a.context_task_id.clone();
    let len_before = a.messages.len();

    a.finish_agent_turn("the real answer".into(), Some("late-task".into()));

    assert_eq!(
        a.context_task_id, ctx_before,
        "stale task id must not be threaded"
    );
    assert_eq!(a.messages.len(), len_before, "no phantom message appended");
    assert_eq!(
        a.messages.last().unwrap().text,
        "partial",
        "bubble keeps partial text"
    );
}

#[test]
fn append_delta_preserves_user_scroll() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.scroll_back = 7;
    a.append_delta("tok", false);
    assert_eq!(
        a.scroll_back, 7,
        "streaming must not yank the viewport to bottom"
    );
}

#[test]
fn finished_agent_turn_caches_markdown() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.finish_agent_turn("**bold**".into(), Some("t1".into()));
    assert!(a.messages.last().unwrap().rendered.is_some());
}

#[test]
fn finishing_a_turn_moves_the_settlement_off_the_body() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.finish_agent_turn(
        "did it\n\n```\n─ settlement ─\n  ✔ bash · cargo test\n```".to_string(),
        None,
    );
    let m = a.messages.last().expect("a message");
    assert_eq!(m.text, "did it", "card must not stay in the body");
    assert_eq!(
        m.settlement.as_deref(),
        Some("  ✔ bash · cargo test"),
        "card must be carried separately so it can be drawn at the pane width"
    );
}

#[test]
fn finish_agent_turn_survives_mid_turn_system_note() {
    // Regression (#6): a system note pushed while streaming (e.g. HITL
    // "approved `bash`") lands after the agent bubble; the turn's finish
    // must still find that bubble instead of silently dropping the reply.
    let mut a = app();
    let tid = a.begin_user_turn("run ls");
    a.append_delta("partial", false);
    a.push_system("approved `bash`");
    a.append_delta(" more", false);
    a.finish_agent_turn("final reply".into(), Some(tid.clone()));
    let agent = a
        .messages
        .iter()
        .find(|m| m.role == Role::Agent)
        .expect("agent bubble");
    assert_eq!(agent.text, "final reply");
    assert!(!agent.streaming);
    assert_eq!(a.context_task_id.as_deref(), Some(tid.as_str()));
    assert!(!a.streaming);
}

#[test]
fn fail_turn_drops_displaced_streaming_placeholder() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.push_system("note lands after the placeholder");
    a.fail_turn("boom");
    assert!(
        !a.messages
            .iter()
            .any(|m| m.role == Role::Agent && m.streaming)
    );
    assert!(!a.streaming);
}

#[test]
fn fail_turn_drops_streaming_placeholder() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.fail_turn("boom");
    // user msg + system error; the empty agent placeholder is removed.
    assert_eq!(a.messages.last().unwrap().role, Role::System);
    assert!(!a.streaming);
}

#[test]
fn new_session_resets_context() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.finish_agent_turn("ok".into(), Some("t1".into()));
    let s = Session::create(&a.home, &a.agent).unwrap();
    a.start_new_session(s);
    assert!(a.context_task_id.is_none());
    assert_eq!(a.messages.last().unwrap().role, Role::System);
}

#[test]
fn effort_parses_bare_level_and_save() {
    assert_eq!(
        parse_slash("/effort"),
        Some(SlashCmd::Effort {
            level: None,
            save: false
        })
    );
    assert_eq!(
        parse_slash("/effort high"),
        Some(SlashCmd::Effort {
            level: Some("high".into()),
            save: false
        })
    );
    assert_eq!(
        parse_slash("/effort high --save"),
        Some(SlashCmd::Effort {
            level: Some("high".into()),
            save: true
        })
    );
    // Order must not matter, and `--save` alone is a listing, not a write.
    assert_eq!(
        parse_slash("/effort --save xhigh"),
        Some(SlashCmd::Effort {
            level: Some("xhigh".into()),
            save: true
        })
    );
    assert_eq!(
        parse_slash("/effort --save"),
        Some(SlashCmd::Effort {
            level: None,
            save: true
        })
    );
}

#[test]
fn parse_slash_model() {
    assert_eq!(parse_slash("/model"), Some(SlashCmd::Model(None)));
    assert_eq!(
        parse_slash("/model 2"),
        Some(SlashCmd::Model(Some("2".into())))
    );
    assert_eq!(
        parse_slash("/model claude_opus"),
        Some(SlashCmd::Model(Some("claude_opus".into())))
    );
}

#[test]
fn parse_slash_login() {
    assert_eq!(parse_slash("/login"), Some(SlashCmd::Login(None)));
    assert_eq!(
        parse_slash("/login anthropic"),
        Some(SlashCmd::Login(Some("anthropic".into())))
    );
    assert_eq!(
        parse_slash("/login chatgpt"),
        Some(SlashCmd::Login(Some("chatgpt".into())))
    );
    // The word is kept verbatim: an unknown provider is reported with the
    // spelling the user typed, not silently dropped.
    assert_eq!(
        parse_slash("/login bogus"),
        Some(SlashCmd::Login(Some("bogus".into())))
    );
}

#[test]
fn parse_slash_secret() {
    let s = |key: Option<&str>, delete| {
        Some(SlashCmd::Secret {
            key: key.map(str::to_string),
            delete,
        })
    };
    assert_eq!(parse_slash("/secret"), s(None, false));
    assert_eq!(
        parse_slash("/secret GITEA_TOKEN"),
        s(Some("GITEA_TOKEN"), false)
    );
    assert_eq!(
        parse_slash("/secret GITEA_TOKEN --delete"),
        s(Some("GITEA_TOKEN"), true)
    );
    assert_eq!(
        parse_slash("/secret --delete GITEA_TOKEN"),
        s(Some("GITEA_TOKEN"), true)
    );
    // Only the KEY is taken. A value typed on this line is ignored rather
    // than accepted, so no path exists where a secret rides in the
    // composer.
    assert_eq!(
        parse_slash("/secret GITEA_TOKEN somevalue"),
        s(Some("GITEA_TOKEN"), false)
    );
}

#[test]
fn parse_slash_channels() {
    let plain = |n| Some(SlashCmd::Channels { n, follow: false });
    assert_eq!(parse_slash("/channels"), plain(None));
    assert_eq!(parse_slash("/channels 2"), plain(Some(2)));
    assert_eq!(parse_slash("/chan"), plain(None));
    assert_eq!(parse_slash("/channels x"), plain(None));
    // The flag is order-independent, and `-f` is the short form. No N with
    // `--follow` means "stop following".
    for line in ["/channels 2 --follow", "/channels --follow 2", "/chan -f 2"] {
        assert_eq!(
            parse_slash(line),
            Some(SlashCmd::Channels {
                n: Some(2),
                follow: true
            }),
            "{line}"
        );
    }
    assert_eq!(
        parse_slash("/channels --follow"),
        Some(SlashCmd::Channels {
            n: None,
            follow: true
        })
    );
}

#[test]
fn input_history_recall_cycle() {
    let home = tempdir().unwrap();
    let mut a = app_at(&home);
    a.history_record("first");
    a.history_record("second");
    a.history_record("second"); // consecutive dup skipped
    assert_eq!(a.sent_history.len(), 2);

    a.set_input("draft");
    assert!(a.history_prev());
    assert_eq!(a.input_text(), "second");
    assert!(a.history_prev());
    assert_eq!(a.input_text(), "first");
    assert!(a.history_prev()); // at oldest — swallowed, unchanged
    assert_eq!(a.input_text(), "first");
    assert!(a.history_next());
    assert_eq!(a.input_text(), "second");
    assert!(a.history_next()); // past newest — draft restored
    assert_eq!(a.input_text(), "draft");
    assert!(!a.history_next()); // not browsing — key not consumed
}

#[test]
fn switch_channel_loads_history_and_caches_meta() {
    let home = tempdir().unwrap();
    let mut a = app_at(&home);
    a.begin_user_turn("first question");
    a.finish_agent_turn("first answer".into(), Some("t1".into()));
    let first_id = a.channel.as_ref().expect("channel after turn").id.clone();

    // Start a new (second) session — channel should be cleared.
    let s = Session::create(&a.home, &a.agent).unwrap();
    a.start_new_session(s);
    assert!(a.channel.is_none(), "channel cleared after new session");

    // Switch back to the first channel.
    a.switch_channel(&first_id).unwrap();
    assert_eq!(a.channel.as_ref().unwrap().id, first_id);
    assert!(
        a.messages.iter().any(|m| m.text == "first question"),
        "history rehydrated after switch"
    );
}

#[test]
fn start_new_session_clears_last_sent() {
    let home = tempdir().unwrap();
    let mut a = app_at(&home);
    a.last_sent = Some("hello".into());
    a.last_esc_at = Some(std::time::Instant::now());
    a.esc_hint = true;
    let s = Session::create(home.path(), "a").unwrap();
    a.start_new_session(s);
    assert!(a.last_sent.is_none());
    assert!(a.last_esc_at.is_none());
    assert!(!a.esc_hint);
}
