//! #2531: a fleet run that cannot sign is refused before it starts.

use super::*;
use crate::cmd::fleet::run::{RunCwd, cmd_fleet_run};

fn fleet(router: Option<&str>) -> Fleet {
    let mut f: Fleet =
        serde_yaml_ng::from_str("name: dev\nchannel_id: fleet-dev\nmembers: [coder]\n").unwrap();
    f.router = router.map(str::to_string);
    f
}

#[test]
fn a_run_whose_signers_are_all_readable_passes() {
    assert!(check(&fleet(None), |_| None).is_ok());
}

#[test]
fn an_unreadable_control_plane_key_refuses_and_names_the_tool() {
    let err = check(&fleet(None), |a| {
        (a == CONTROL_PLANE_AGENT).then(|| format!("'{a}' unreadable"))
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("refusing to run fleet 'dev'"), "{err}");
    assert!(err.contains("`fleet_run` tool"), "{err}");
}

#[test]
fn a_custom_router_is_checked_as_well_as_the_control_plane() {
    let err = check(&fleet(Some("orchestrator")), |a| {
        (a == "orchestrator").then(|| format!("'{a}' unreadable"))
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("'orchestrator' unreadable"), "{err}");
}

/// The shell path of #2531, reproduced the way the sandbox does it: the
/// control-plane key is there and this process may not read it, and no
/// handoff covers it. The run must stop before it queues the job, takes the
/// run lock, or writes a single channel event.
#[cfg(unix)]
#[tokio::test]
async fn a_one_shot_run_that_cannot_sign_leaves_no_trace() {
    use std::os::unix::fs::PermissionsExt;
    // Root reads a mode-000 file, so the denial this test needs never happens.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let f = fleet(None);
    crate::cmd::fleet::store::save_fleet(home, &f).unwrap();
    let key = mur_common::identity::private_key_dir(&home.join("agents").join(CONTROL_PLANE_AGENT))
        .join("identity.key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
    let res = cmd_fleet_run(
        home,
        "dev",
        Some("fix the build".into()),
        false,
        None,
        RunCwd::default(),
    )
    .await;
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let err = format!("{:#}", res.expect_err("an unsignable run must not start"));
    assert!(err.contains("refusing to run fleet 'dev'"), "{err}");
    assert!(
        crate::cmd::fleet::jobs::list_jobs_raw(home, "dev")
            .unwrap_or_default()
            .is_empty(),
        "no job may be queued by a refused run"
    );
    assert!(
        !crate::cmd::fleet::loop_run::run_lock::is_held(home, "dev"),
        "the refused run holds no lock"
    );
}

/// The other half of #2531: a one-shot run takes the run lock, so a second
/// run of the same fleet is refused while one is live.
#[tokio::test]
async fn a_one_shot_run_is_refused_while_another_run_is_live() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::cmd::fleet::store::save_fleet(home, &fleet(None)).unwrap();
    let _live = crate::cmd::fleet::loop_run::run_lock::acquire(home, "dev").unwrap();
    let err = cmd_fleet_run(
        home,
        "dev",
        Some("x".into()),
        false,
        None,
        RunCwd::default(),
    )
    .await
    .expect_err("a second live run must be refused");
    assert!(format!("{err:#}").contains("is live"), "{err:#}");
    assert!(
        crate::cmd::fleet::jobs::list_jobs_raw(home, "dev")
            .unwrap_or_default()
            .is_empty(),
        "the refused run queued nothing"
    );
}
