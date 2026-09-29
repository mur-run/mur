//! Regression: a mid-session `/skill add` (and its `/browser --add` caller)
//! must refresh `app.skills`, not leave the startup cache stale.
//!
//! `complete::load_agent_skills` is called exactly once, at TUI startup
//! (`term.rs`), and its own doc comment admits the gap: "cached once at
//! startup; mid-session `/skill add` won't refresh it." The user-visible
//! symptom is `/browser --add` succeeding — the skill really is written into
//! the profile — and the very next `/browser` still answering "browser skill
//! not attached — run /browser --add first", because that guard reads the
//! stale `app.skills`.
//!
//! These tests drive the real dispatch path (`browser_cmd::handle`, which
//! calls `slash_cmds::run_manage` -> `manage::skill_add` -> `cmd_skill_add`)
//! against a temp MUR home, so nothing here is a mock of the thing under
//! test.

use super::super::stream::STREAM_CHANNEL_CAP;
use super::super::*;

use mur_common::agent::AgentProfile;

/// A temp MUR home with a real agent profile and the global `browser`
/// SKILL.md that `/browser --add` installs from.
fn fixture_home(agent: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join(agent);
    std::fs::create_dir_all(&dir).unwrap();
    let profile = AgentProfile {
        name: agent.to_string(),
        ..AgentProfile::default_for_tests()
    };
    std::fs::write(
        dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();

    // The global copy `browser_cmd::handle` reads: `<home>/skills/browser/SKILL.md`,
    // shipped in production by `ensure_mur_skill`.
    let global = home.path().join("skills").join("browser");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(
        global.join("SKILL.md"),
        "---\n\
         name: browser\n\
         version: 1.0.0\n\
         publisher: human:test\n\
         category: workflow\n\
         description: Browser automation hub skill for the refresh regression test.\n\
         ---\n\n\
         # browser\n\n\
         Body text for the browser skill.\n",
    )
    .unwrap();
    home
}

fn app_for(home: &std::path::Path, agent: &str) -> App {
    let session = Session::create(home, agent).unwrap();
    let mut app = App::new(
        home.to_path_buf(),
        agent.to_string(),
        session,
        &super::super::theme::ANSI,
    );
    // Exactly what `term.rs` does once at startup.
    app.skills = complete::load_agent_skills(&app.agent);
    app
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn browser_add_refreshes_the_skill_menu_in_the_same_session() {
    let agent = "skillrefresh";
    let home = fixture_home(agent);

    // `load_agent_skills` and `cmd_skill_add` both resolve the MUR home
    // themselves via `resolve_mur_home()`, so the tempdir must be handed to
    // them through the env var. Requires `--test-threads=1` for this file's
    // env mutation, per the workspace convention.
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", home.path());

    let mut app = app_for(home.path(), agent);
    assert!(
        !app.skills.iter().any(|c| c.display == "/browser"),
        "precondition: the fixture starts with no browser skill attached"
    );

    let (tx, _rx) = mpsc::channel(STREAM_CHANNEL_CAP);
    block_on(browser_cmd::handle(
        &mut app,
        vec!["--add".to_string()],
        &tx,
    ));

    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("installed skill")),
        "the add itself must have succeeded first: {:?}",
        app.messages
            .iter()
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        app.skills.iter().any(|c| c.display == "/browser"),
        "after a successful /browser --add, the same session's skill menu must \
         list /browser — a stale cache is what makes the next /browser wrongly \
         say 'not attached'. got: {:?}",
        app.skills
            .iter()
            .map(|c| c.display.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn browser_mode_arg_right_after_add_no_longer_refuses() {
    // The user-visible bug, end to end: add, then immediately invoke. The
    // second call must get past the `not attached` guard.
    let agent = "skillrefresh2";
    let home = fixture_home(agent);
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", home.path());

    let mut app = app_for(home.path(), agent);
    let (tx, _rx) = mpsc::channel(STREAM_CHANNEL_CAP);

    block_on(browser_cmd::handle(
        &mut app,
        vec!["--add".to_string()],
        &tx,
    ));
    let after_add = app.messages.len();
    block_on(browser_cmd::handle(
        &mut app,
        vec!["testing".to_string()],
        &tx,
    ));

    assert!(
        !app.messages[after_add..]
            .iter()
            .any(|m| m.text.contains("not attached")),
        "invoking the skill right after attaching it must not hit the refusal \
         branch: {:?}",
        app.messages[after_add..]
            .iter()
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
    );
}
