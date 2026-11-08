//! #2316: a fleet run's checkpoint and record are read from that fleet's own
//! tree only. `runs/` is writable by every sandboxed agent, so a pause it
//! holds must never be trusted by `mur fleet resume`.

use super::*;
use crate::run_status::{RUN_SCHEMA, RunKind, RunState, State};

const RUN: &str = "fleet-dev-1";

fn fleet() -> mur_common::fleet::Fleet {
    serde_yaml_ng::from_str("name: dev\nchannel_id: fleet-dev\nmembers: [pm]\ngoal: g\n").unwrap()
}

fn paused_record() -> RunState {
    let now = chrono::Utc::now();
    RunState {
        schema: RUN_SCHEMA,
        run_id: RUN.into(),
        channel_id: Some("fleet-dev".into()),
        kind: RunKind::Fleet,
        label: "fleet dev loop".into(),
        pid: 1,
        started_at: now,
        last_heartbeat_at: Some(now),
        state: State::Paused,
        steps: vec![],
        blocked_on: None,
        binary_version: "test".into(),
        build_sha: String::new(),
    }
}

fn with_sentinel() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    store::save_fleet(tmp.path(), &fleet()).unwrap();
    std::fs::write(
        control::paused_path(tmp.path(), "dev"),
        format!("run_id: {RUN}\nrequested_at: t\n"),
    )
    .unwrap();
    tmp
}

/// Write a checkpoint + record straight into `runs/<run_id>/`, the way any
/// sandboxed agent can (#2316's attack).
fn forge_in_runs(home: &Path) {
    let dir = home.join(mur_common::paths::RUNS).join(RUN);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cp = checkpoint::sample_for_test(RUN);
    cp.cost_usd = Some(1_000_000.0);
    std::fs::write(
        dir.join(checkpoint::CHECKPOINT_FILE),
        serde_json::to_vec(&cp).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("run.json"),
        serde_json::to_vec(&paused_record()).unwrap(),
    )
    .unwrap();
}

#[test]
fn a_pause_forged_in_runs_is_not_resumable() {
    let home = with_sentinel();
    forge_in_runs(home.path());
    let err = admit(home.path(), "dev").expect_err("a forged pause must not be admitted");
    assert!(format!("{err:#}").contains("no checkpoint"), "{err:#}");
}

#[test]
fn a_real_pause_is_written_and_read_in_the_fleet_tree() {
    let home = with_sentinel();
    let h = home.path();
    checkpoint::save(h, &checkpoint::sample_for_test(RUN)).unwrap();
    crate::run_status::store::save(h, &paused_record()).unwrap();

    let tree = mur_common::paths::fleet_run_dir(h, "dev", RUN);
    assert!(tree.join(checkpoint::CHECKPOINT_FILE).exists());
    assert!(tree.join("run.json").exists());
    assert!(
        !h.join(mur_common::paths::RUNS).join(RUN).exists(),
        "nothing in runs/"
    );
    assert_eq!(admit(h, "dev").expect("resumable").run_id, RUN);
}

/// The forged copy in `runs/` cannot override the real one either.
#[test]
fn a_forged_copy_beside_the_real_pause_is_ignored() {
    let home = with_sentinel();
    let h = home.path();
    checkpoint::save(h, &checkpoint::sample_for_test(RUN)).unwrap();
    crate::run_status::store::save(h, &paused_record()).unwrap();
    forge_in_runs(h);
    assert_eq!(admit(h, "dev").unwrap().cost_usd, Some(5.0));
}

/// A checkpoint naming one fleet is never written into another's tree.
#[test]
fn a_checkpoint_is_saved_under_the_fleet_it_names() {
    let home = tempfile::tempdir().unwrap();
    let mut cp = checkpoint::sample_for_test(RUN);
    cp.fleet = "../x".into();
    assert!(checkpoint::save(home.path(), &cp).is_err());
}
