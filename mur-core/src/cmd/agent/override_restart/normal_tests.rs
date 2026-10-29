use mur_common::startup_override::claim::{consumed_path, live_path};

use super::super::outcome::Step;
use super::super::test_support::*;
use super::*;

const ORPHAN: &str = "ovr-00000000000000000000000000000000";

fn running(home: &Home) {
    let mut lock = base_lock(OLD_PID);
    // An unknown old build skips the CLI's "came back on the SAME binary"
    // upgrade verdict, which compares against the real on-disk runtime.
    lock.build_sha = "unknown".into();
    std::fs::write(home.lock_path(), serde_json::to_vec(&lock).unwrap()).unwrap();
}

fn plant_orphan(home: &Home) {
    std::fs::create_dir_all(home.dir()).unwrap();
    std::fs::write(
        live_path(&home.dir(), NAME),
        format!(r#"{{"nonce":"{ORPHAN}"}}"#),
    )
    .unwrap();
}

#[test]
fn an_orphan_is_swept_and_the_restart_boots_without_it() {
    let home = Home::new();
    running(&home);
    plant_orphan(&home);
    // `Boot::Ignores` never claims; a live file at boot would still be there.
    let mut fake = Fake::new(&home, Boot::Ignores);
    let out = restart_normally(
        home.prepared(super::super::Mode::Restart),
        Some("dashboard".into()),
        &mut fake,
        &mut |_| {},
    )
    .unwrap();
    assert!(out.ok, "{out:?}");
    assert_eq!(fake.calls, ["stop", "start"]);
    assert!(!live_path(&home.dir(), NAME).exists(), "orphan swept");
    assert!(
        !consumed_path(&home.dir(), NAME).exists(),
        "nothing consumed"
    );
    let audit = home.audit();
    assert!(audit.contains("startup_override_discarded"), "{audit}");
    assert!(audit.contains(ORPHAN), "{audit}");
    // It never publishes.
    assert!(!audit.contains("startup_override_issued"), "{audit}");
    assert!(home.lock_is_free());
}

#[cfg(unix)]
#[test]
fn a_sweep_that_fails_restarts_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    running(&home);
    plant_orphan(&home);
    // The writer lock is taken first, as `prepare` would.
    let prepared = home.prepared(super::super::Mode::Restart);
    std::fs::set_permissions(home.dir(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut fake = Fake::new(&home, Boot::Ignores);
    let out = restart_normally(prepared, None, &mut fake, &mut |_| {});
    std::fs::set_permissions(home.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    match out {
        Err(OverrideOutcome::Failed {
            step: Step::Sweep, ..
        }) => {}
        other => panic!("{other:?}"),
    }
    assert!(fake.calls.is_empty(), "{:?}", fake.calls);
}
