use super::*;

use std::path::PathBuf;

fn legacy_run(home: &Path, run_id: &str, files: &[(&str, serde_json::Value)]) -> PathBuf {
    let dir = home.join(mur_common::paths::RUNS).join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        std::fs::write(dir.join(name), body.to_string()).unwrap();
    }
    dir
}

fn fleet_record(channel: &str) -> serde_json::Value {
    serde_json::json!({"kind": "fleet", "channel_id": channel})
}

#[test]
fn a_fleet_run_moves_into_its_fleet_tree_and_a_job_run_stays() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    legacy_run(h, "fleet-dev-1", &[(RUN_FILE, fleet_record("fleet-dev"))]);
    legacy_run(
        h,
        "run-job",
        &[(
            RUN_FILE,
            serde_json::json!({"kind": "job", "channel_id": "fleet-dev"}),
        )],
    );

    migrate_once(h);

    let moved = mur_common::paths::fleet_run_dir(h, "dev", "fleet-dev-1");
    assert!(moved.join(RUN_FILE).exists(), "fleet run moved");
    assert!(!h.join("runs/fleet-dev-1").exists(), "nothing left behind");
    assert!(
        h.join("runs/run-job").exists(),
        "a job run is not a fleet's"
    );
}

#[test]
fn the_fleet_is_read_from_the_checkpoint_when_the_record_is_gone() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    legacy_run(
        h,
        "fleet-dev-2",
        &[(
            crate::cmd::fleet::loop_run::checkpoint::CHECKPOINT_FILE,
            serde_json::json!({"fleet": "dev"}),
        )],
    );
    migrate_once(h);
    assert!(mur_common::paths::fleet_run_dir(h, "dev", "fleet-dev-2").exists());
}

/// The marker makes it once: a run planted in `runs/` after the move is never
/// adopted into a fleet's tree, where `mur fleet resume` would trust it.
#[test]
fn a_run_planted_after_the_move_is_never_adopted() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    std::fs::create_dir_all(h.join(mur_common::paths::RUNS)).unwrap();
    migrate_once(h);
    assert!(h.join(mur_common::paths::FLEET_STATE).join(MARKER).exists());

    legacy_run(
        h,
        "fleet-dev-evil",
        &[(RUN_FILE, fleet_record("fleet-dev"))],
    );
    migrate_once(h);

    assert!(!mur_common::paths::fleet_run_dir(h, "dev", "fleet-dev-evil").exists());
    assert!(h.join("runs/fleet-dev-evil").exists());
}

/// The fleet tree is the trusted copy: a clash leaves both in place.
#[test]
fn an_existing_fleet_run_is_never_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let trusted = mur_common::paths::fleet_run_dir(h, "dev", "fleet-dev-3");
    std::fs::create_dir_all(&trusted).unwrap();
    std::fs::write(trusted.join(RUN_FILE), "trusted").unwrap();
    legacy_run(h, "fleet-dev-3", &[(RUN_FILE, fleet_record("fleet-dev"))]);

    migrate_once(h);

    assert_eq!(
        std::fs::read_to_string(trusted.join(RUN_FILE)).unwrap(),
        "trusted"
    );
    assert!(h.join("runs/fleet-dev-3").exists());
}

/// A name that cannot be a fleet never becomes a path.
#[test]
fn a_traversing_fleet_name_is_ignored() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    legacy_run(h, "fleet-x-1", &[(RUN_FILE, fleet_record("fleet-../etc"))]);
    migrate_once(h);
    assert!(h.join("runs/fleet-x-1").exists());
}

/// A fresh home is claimed at once, so a fleet run an agent plants in `runs/`
/// later is never adopted.
#[test]
fn a_fresh_home_is_claimed_before_anything_can_be_planted() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    migrate_once(h);
    assert!(h.join(mur_common::paths::FLEET_STATE).join(MARKER).exists());
    legacy_run(
        h,
        "fleet-dev-late",
        &[(RUN_FILE, fleet_record("fleet-dev"))],
    );
    migrate_once(h);
    assert!(!mur_common::paths::fleet_run_dir(h, "dev", "fleet-dev-late").exists());
}
