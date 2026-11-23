//! A one-shot run holds the run lock (#2531), so it must refuse to start over
//! a paused run exactly as `--loop` does.

use super::tests::paused_home;
use super::*;
use crate::cmd::fleet::run::{RunCwd, cmd_fleet_run};

#[tokio::test]
async fn a_one_shot_run_over_a_paused_run_is_refused_and_leaves_it_resumable() {
    let home = paused_home(None);
    let err = cmd_fleet_run(
        home.path(),
        "dev",
        Some("x".into()),
        false,
        None,
        RunCwd::default(),
    )
    .await
    .expect_err("a one-shot must not start over a paused run");
    let msg = format!("{err:#}");
    assert!(msg.contains("mur fleet resume dev"), "{msg}");
    assert!(
        control::paused_path(home.path(), "dev").exists(),
        "the pause is kept"
    );
    assert!(
        crate::cmd::fleet::jobs::list_jobs_raw(home.path(), "dev")
            .unwrap_or_default()
            .is_empty(),
        "the refused run queued nothing"
    );
    assert!(
        !run_lock::is_held(home.path(), "dev"),
        "the refused run released the lock"
    );
}
