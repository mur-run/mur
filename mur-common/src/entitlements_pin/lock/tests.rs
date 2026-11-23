use super::*;
use crate::lock_file::LOCK_POLL;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SHORT: Duration = Duration::from_millis(150);
const SLACK: Duration = Duration::from_millis(300);
/// Env var that turns [`holder_child`] from a no-op into a lock holder.
const HOLD_ENV: &str = "MUR_TEST_HOLD_PROFILE_LOCK";
const HELD_MARK: &str = "PROFILE-LOCK-HELD";

/// Not a test of its own: the body of the child process that
/// `lock_profile_until_times_out_while_another_process_holds_the_lock`
/// starts. Without [`HOLD_ENV`] it returns at once.
#[test]
fn holder_child() {
    let Ok(home) = std::env::var(HOLD_ENV) else {
        return;
    };
    let _held = lock_profile(Path::new(&home), "a", || {}).expect("child takes the lock");
    println!("{HELD_MARK}");
    // Hold until the parent closes our stdin (or dies).
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
}

struct Holder(Child);
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn hold_in_another_process(home: &Path) -> Holder {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "entitlements_pin::lock::tests::holder_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HOLD_ENV, home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let holder = Holder(child);
    for line in BufReader::new(out).lines() {
        if line.unwrap().contains(HELD_MARK) {
            return holder;
        }
    }
    panic!("the child exited without taking the lock");
}

#[test]
fn lock_profile_until_takes_a_free_lock_and_records_the_agent() {
    let home = tempfile::tempdir().unwrap();
    let lock = lock_profile_until(home.path(), "a", Instant::now() + SHORT).unwrap();
    assert_eq!(lock.agent(), "a");
    assert_eq!(lock.mur_home(), home.path());
    // Not reentrant, so a second take on the same agent waits it out.
    let again = lock_profile_until(home.path(), "a", Instant::now() + SHORT);
    assert!(matches!(again, Err(PinError::Timeout { .. })));
    // Another agent has its own lock.
    lock_profile_until(home.path(), "b", Instant::now() + SHORT).unwrap();
}

#[test]
fn lock_profile_until_refuses_a_bad_agent_name() {
    let home = tempfile::tempdir().unwrap();
    for bad in ["", "..", "a/b", ".purged.x.1"] {
        let r = lock_profile_until(home.path(), bad, Instant::now() + SHORT);
        assert!(matches!(r, Err(PinError::Name(_))), "{bad:?}");
    }
}

#[test]
fn lock_profile_until_times_out_while_another_process_holds_the_lock() {
    let home = tempfile::tempdir().unwrap();
    let holder = hold_in_another_process(home.path());
    let started = Instant::now();
    let err = lock_profile_until(home.path(), "a", started + SHORT).unwrap_err();
    let waited = started.elapsed();
    let PinError::Timeout { path } = &err else {
        panic!("expected a timeout, got {err:?}");
    };
    assert_eq!(path, &lock_path(home.path(), "a"));
    assert!(waited >= SHORT, "returned early: {waited:?}");
    assert!(waited < SHORT + LOCK_POLL + SLACK, "too late: {waited:?}");
    // The holder going away frees it.
    drop(holder);
    lock_profile_until(home.path(), "a", Instant::now() + Duration::from_secs(5)).unwrap();
}
