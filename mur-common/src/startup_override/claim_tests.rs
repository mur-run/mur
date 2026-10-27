use super::claim::*;
use std::path::Path;
use std::sync::{Arc, Barrier};

const AGENT: &str = "alpha";

fn publish(dir: &Path, body: &str) {
    std::fs::write(live_path(dir, AGENT), body).unwrap();
}

#[test]
fn overrides_dir_sits_under_secrets() {
    let home = Path::new("/h");
    assert_eq!(
        overrides_dir(home),
        Path::new("/h/secrets/startup-overrides")
    );
    let d = Path::new("/d");
    assert_eq!(live_path(d, AGENT), Path::new("/d/alpha.json"));
    assert_eq!(consumed_path(d, AGENT), Path::new("/d/alpha.consumed.json"));
    assert_eq!(
        withdrawn_path(d, AGENT),
        Path::new("/d/alpha.withdrawn.json")
    );
}

#[test]
fn claim_with_no_live_file_is_absent() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        claim(tmp.path(), AGENT, ClaimAs::Consume),
        Claimed::Absent
    ));
    // A missing directory is the common case on a machine that never used overrides.
    let missing = tmp.path().join("nope");
    assert!(matches!(
        claim(&missing, AGENT, ClaimAs::Consume),
        Claimed::Absent
    ));
}

#[test]
fn consume_moves_the_file_to_its_own_name() {
    let tmp = tempfile::tempdir().unwrap();
    publish(tmp.path(), "v1");
    match claim(tmp.path(), AGENT, ClaimAs::Consume) {
        Claimed::Took(p) => {
            assert_eq!(p, consumed_path(tmp.path(), AGENT));
            assert_eq!(std::fs::read_to_string(p).unwrap(), "v1");
        }
        other => panic!("expected Took, got {other:?}"),
    }
    assert!(!live_path(tmp.path(), AGENT).exists());
}

#[test]
fn consume_replaces_a_previous_consumed_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(consumed_path(tmp.path(), AGENT), "old").unwrap();
    publish(tmp.path(), "new");
    assert!(matches!(
        claim(tmp.path(), AGENT, ClaimAs::Consume),
        Claimed::Took(_)
    ));
    let body = std::fs::read_to_string(consumed_path(tmp.path(), AGENT)).unwrap();
    assert_eq!(body, "new");
}

#[test]
fn invalid_agent_name_never_touches_the_fs() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("x.json");
    std::fs::write(&outside, "bait").unwrap();
    let dir = tmp.path().join("inner");
    std::fs::create_dir(&dir).unwrap();
    match claim(&dir, "../x", ClaimAs::Consume) {
        Claimed::Failed(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput),
        other => panic!("expected Failed(InvalidInput), got {other:?}"),
    }
    assert!(outside.exists());
    assert!(!tmp.path().join("x.consumed.json").exists());
}

#[cfg(unix)]
#[test]
fn claim_rename_failure_is_failed_not_absent() {
    use std::os::unix::fs::PermissionsExt;
    // Root ignores directory write bits, so the rename would succeed.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIP: running as root");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    publish(tmp.path(), "v1");
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let got = claim(tmp.path(), AGENT, ClaimAs::Consume);
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(got, Claimed::Failed(_)), "got {got:?}");
    assert!(live_path(tmp.path(), AGENT).exists());
}

/// Another process holding the live file open with no sharing (an antivirus
/// scan, a backup agent) outlasts the retry window. The claim must come back
/// `Failed` and leave the file in place — never `Absent`, which would read as
/// "nothing was published".
#[cfg(windows)]
#[test]
fn claim_blocked_by_sharing_violation_is_failed_not_absent() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

    let tmp = tempfile::tempdir().unwrap();
    publish(tmp.path(), "v1");
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(live_path(tmp.path(), AGENT))
        .unwrap();
    let got = claim(tmp.path(), AGENT, ClaimAs::Consume);
    drop(holder);
    match got {
        Claimed::Failed(e) => assert_eq!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32)),
        other => panic!("expected Failed(sharing violation), got {other:?}"),
    }
    assert!(live_path(tmp.path(), AGENT).exists());
    assert!(!consumed_path(tmp.path(), AGENT).exists());
}

#[test]
fn claim_race_exactly_one_wins() {
    const ITERATIONS: usize = 500;
    let tmp = tempfile::tempdir().unwrap();
    let dir = Arc::new(tmp.path().to_path_buf());
    for i in 0..ITERATIONS {
        let _ = std::fs::remove_file(consumed_path(&dir, AGENT));
        let _ = std::fs::remove_file(withdrawn_path(&dir, AGENT));
        publish(&dir, &format!("{i}"));

        let barrier = Arc::new(Barrier::new(2));
        let spawn = |who: ClaimAs| {
            let (dir, barrier) = (dir.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                claim(&dir, AGENT, who)
            })
        };
        let (consume, withdraw) = if i % 2 == 0 {
            let c = spawn(ClaimAs::Consume);
            (c, spawn(ClaimAs::Withdraw))
        } else {
            let w = spawn(ClaimAs::Withdraw);
            (spawn(ClaimAs::Consume), w)
        };
        let (c, w) = (consume.join().unwrap(), withdraw.join().unwrap());

        let took = |r: &Claimed| matches!(r, Claimed::Took(_));
        let absent = |r: &Claimed| matches!(r, Claimed::Absent);
        assert!(
            (took(&c) && absent(&w)) || (absent(&c) && took(&w)),
            "iteration {i}: consume={c:?} withdraw={w:?}"
        );
        assert!(!live_path(&dir, AGENT).exists(), "iteration {i}");
        if took(&c) {
            // The withdraw side never removes what the boot consumed.
            let body = std::fs::read_to_string(consumed_path(&dir, AGENT)).unwrap();
            assert_eq!(body, format!("{i}"), "iteration {i}");
        }
    }
}
