use std::sync::{Arc, Mutex};

use mur_common::startup_override::claim::live_path;

use super::super::test_support::*;
use super::super::{AlwaysYes, JobGate, Mode, PublishPermit};
use super::*;

fn run(home: &Home, mode: Mode, fake: &mut Fake) -> OverrideOutcome {
    override_restart(
        home.prepared(mode),
        issuer(),
        Box::new(AlwaysYes),
        fake,
        &mut |_| {},
    )
}

fn unjam(home: &Home) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn applied_on_time_for_restart() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Applies);
    let out = run(&home, Mode::Restart, &mut fake);
    assert_eq!(
        out,
        OverrideOutcome::Applied {
            flags: flags(),
            late: false,
            detail: None
        }
    );
    assert_eq!(fake.calls, ["stop", "start"]);
    assert!(home.audit().contains("startup_override_issued"));
    assert!(home.lock_is_free());
}

#[test]
fn start_mode_never_stops() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Applies);
    let out = run(&home, Mode::Start, &mut fake);
    assert!(out.is_success(), "{out:?}");
    assert_eq!(fake.calls, ["start"]);
}

#[test]
fn rejected_is_reported_with_the_reason() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Rejects);
    let out = run(&home, Mode::Restart, &mut fake);
    assert_eq!(
        out,
        OverrideOutcome::Rejected {
            reason: "override rejected: stale".into(),
            late: false,
            detail: None
        }
    );
    assert!(!out.is_success());
}

#[test]
fn a_boot_that_ignored_the_file_is_withdrawn() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Ignores);
    let out = run(&home, Mode::Restart, &mut fake);
    match out {
        OverrideOutcome::Failed {
            step: Step::NoAck,
            cleanup: Some(Cleanup::Withdrawn),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert!(
        !live_path(&home.dir(), NAME).exists(),
        "W leaves nothing live"
    );
    assert!(home.audit().contains("startup_override_withdrawn"));
    assert!(home.lock_is_free());
}

#[test]
fn consumed_but_unacknowledged_is_c() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::ConsumesSilently);
    match run(&home, Mode::Restart, &mut fake) {
        OverrideOutcome::Failed {
            step: Step::NoAck,
            cleanup: Some(Cleanup::ConsumedUnacknowledged),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert!(home.audit().contains("startup_override_unacknowledged"));
}

#[cfg(unix)]
#[test]
fn a_withdraw_that_fails_is_f_and_leaves_the_file() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Ignores);
    fake.jam_withdraw = true;
    let out = run(&home, Mode::Restart, &mut fake);
    unjam(&home);
    match out {
        OverrideOutcome::Failed {
            step: Step::NoAck,
            cleanup: Some(Cleanup::WithdrawFailed { .. }),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert!(
        live_path(&home.dir(), NAME).exists(),
        "F leaves the file live"
    );
    // The next writer's sweep discards it.
    let mut next = Fake::new(&home, Boot::Applies);
    assert!(run(&home, Mode::Restart, &mut next).is_success());
    assert!(home.audit().contains("startup_override_discarded"));
}

#[test]
fn stop_failure_then_late_apply_wins_with_detail() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Applies);
    fake.stop_fails = true;
    fake.boot_in_stop = true;
    match run(&home, Mode::Restart, &mut fake) {
        OverrideOutcome::Applied {
            late: true,
            detail: Some(d),
            ..
        } => assert!(d.contains("could not stop"), "{d}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.calls, ["stop"], "no start after a failed stop");
}

#[test]
fn stop_failure_with_no_boot_withdraws() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Applies);
    fake.stop_fails = true;
    match run(&home, Mode::Restart, &mut fake) {
        OverrideOutcome::Failed {
            step: Step::Stop,
            cleanup: Some(Cleanup::Withdrawn),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn start_failure_withdraws() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Applies);
    fake.start_fails = true;
    match run(&home, Mode::Start, &mut fake) {
        OverrideOutcome::Failed {
            step: Step::Start,
            cleanup: Some(Cleanup::Withdrawn),
            detail,
        } => assert!(detail.contains("attestation failed"), "{detail}"),
        other => panic!("{other:?}"),
    }
    assert!(!live_path(&home.dir(), NAME).exists());
}

#[test]
fn an_orphan_is_swept_before_publish() {
    let home = Home::new();
    std::fs::create_dir_all(home.dir()).unwrap();
    std::fs::write(
        live_path(&home.dir(), NAME),
        br#"{"nonce":"ovr-00000000000000000000000000000000"}"#,
    )
    .unwrap();
    let mut fake = Fake::new(&home, Boot::Applies);
    assert!(run(&home, Mode::Restart, &mut fake).is_success());
    let audit = home.audit();
    assert!(audit.contains("startup_override_discarded"));
    assert!(audit.contains("ovr-00000000000000000000000000000000"));
}

#[test]
fn start_rechecks_running_under_the_lock() {
    let home = Home::new();
    std::fs::write(
        home.lock_path(),
        serde_json::to_vec(&base_lock(std::process::id())).unwrap(),
    )
    .unwrap();
    let mut fake = Fake::new(&home, Boot::Applies);
    match run(&home, Mode::Start, &mut fake) {
        OverrideOutcome::Failed {
            step: Step::Refusal { reason },
            cleanup: None,
            ..
        } => assert!(reason.contains("already running"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(fake.calls.is_empty());
    assert!(!live_path(&home.dir(), NAME).exists(), "nothing published");
}

struct Gate {
    publish: bool,
    cleanup: bool,
    committed: Arc<Mutex<Option<String>>>,
}

struct Permit(Arc<Mutex<Option<String>>>);

impl PublishPermit for Permit {
    fn commit(self: Box<Self>, nonce: &str) {
        *self.0.lock().unwrap() = Some(nonce.to_owned());
    }
}

impl JobGate for Gate {
    fn begin_publish(&mut self) -> Option<Box<dyn PublishPermit>> {
        self.publish
            .then(|| Box::new(Permit(self.committed.clone())) as Box<dyn PublishPermit>)
    }
    fn begin_cleanup(&mut self) -> bool {
        self.cleanup
    }
}

fn run_gated(
    home: &Home,
    fake: &mut Fake,
    publish: bool,
    cleanup: bool,
) -> (OverrideOutcome, Option<String>) {
    let committed = Arc::new(Mutex::new(None));
    let gate = Gate {
        publish,
        cleanup,
        committed: committed.clone(),
    };
    let out = override_restart(
        home.prepared(Mode::Restart),
        issuer(),
        Box::new(gate),
        fake,
        &mut |_| {},
    );
    let n = committed.lock().unwrap().clone();
    (out, n)
}

#[test]
fn a_cancelled_gate_publishes_nothing() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Applies);
    let (out, committed) = run_gated(&home, &mut fake, false, true);
    assert!(
        matches!(
            out,
            OverrideOutcome::Failed {
                step: Step::Cancelled,
                cleanup: None,
                ..
            }
        ),
        "{out:?}"
    );
    assert!(committed.is_none());
    assert!(fake.calls.is_empty());
    assert!(!live_path(&home.dir(), NAME).exists());
}

#[test]
fn a_gate_that_takes_cleanup_abandons_and_audits_nothing_more() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Ignores);
    let (out, committed) = run_gated(&home, &mut fake, true, false);
    assert!(matches!(out, OverrideOutcome::Abandoned { .. }), "{out:?}");
    assert!(committed.is_some(), "publish committed its nonce");
    assert!(!home.audit().contains("startup_override_withdrawn"));
    assert!(
        live_path(&home.dir(), NAME).exists(),
        "the quit path owns the claim"
    );
}

#[test]
fn the_writer_lock_is_released_after_a_panic_in_stop() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Applies);
    fake.panic_in_stop = true;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(&home, Mode::Restart, &mut fake)
    }));
    assert!(r.is_err());
    assert!(home.lock_is_free());
}

/// A directory `fsync` that fails after the link leaves the file live: the
/// writer must withdraw it and never launch on a non-durable override.
#[test]
fn an_unsynced_publish_is_withdrawn_and_never_launches() {
    let home = Home::new();
    let mut fake = Fake::new(&home, Boot::Applies);
    writer::DIR_SYNC_HOOK
        .with(|h| *h.borrow_mut() = Some(Box::new(|_| Err(std::io::Error::other("eio")))));
    let out = run(&home, Mode::Restart, &mut fake);
    writer::DIR_SYNC_HOOK.with(|h| *h.borrow_mut() = None);
    match out {
        OverrideOutcome::Failed {
            step: Step::Publish,
            cleanup: Some(Cleanup::Withdrawn),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert!(fake.calls.is_empty(), "{:?}", fake.calls);
    assert!(!live_path(&home.dir(), NAME).exists());
    let audit = home.audit();
    assert!(audit.contains("startup_override_issued"), "{audit}");
    assert!(audit.contains("startup_override_withdrawn"), "{audit}");
    assert!(home.lock_is_free());
}

/// Like plain start/restart (#1980), a confirmed override launch is the
/// user's answer to the keychain prompt, so pending markers are cleared.
#[test]
fn an_override_launch_clears_pending_keychain_markers() {
    let home = Home::new();
    let agent_home = home.path().join("agents").join(NAME);
    mur_common::secret::keychain_pending::mark(&agent_home, "GOOGLE_MAPS_KEY").unwrap();
    let mut fake = Fake::new(&home, Boot::Applies);
    assert!(run(&home, Mode::Restart, &mut fake).is_success());
    assert!(mur_common::secret::keychain_pending::list(&agent_home).is_empty());
}
