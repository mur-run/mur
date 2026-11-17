//! `App::test_fixture`, moved out of `app/mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement.

use super::super::*;

impl App {
    /// Minimal fixture for unit tests. Backed by a temporary directory that
    /// lives as long as the app and holds the agent's writer key, so session
    /// appends succeed even under `MUR_CHANNEL_REQUIRE_SIG` (#2145). That
    /// matters since at-mention C1: a user turn whose append fails is not
    /// sent at all, so a fixture that cannot persist cannot start a turn.
    pub fn test_fixture() -> Self {
        let home = tempfile::tempdir().unwrap();
        crate::channel_writer::plant_identity_for(home.path(), "a");
        let session = Session::create(home.path(), "a").unwrap();
        let mut app = App::new(
            home.path().to_path_buf(),
            "a".into(),
            session,
            &crate::cmd::agent::cli::theme::ANSI,
        );
        app._test_home = Some(home);
        app
    }
}
