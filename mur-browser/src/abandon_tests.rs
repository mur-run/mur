use super::*;
use chrono::DateTime;

fn setup(home: &Path, site: &str) {
    let meta = auth::ProfileMeta {
        url: format!("https://{site}.example/login"),
        last_auth: DateTime::from_timestamp(1_600_000_000, 0).unwrap(),
        earliest_cookie_expires: None,
        allow_domains: vec![],
        last_refresh: None,
        writeback_abandoned: None,
    };
    auth::write_meta(&paths::profile_meta(home, site), &meta).unwrap();
}

fn overlap(at: i64, host: &str) -> Abandoned {
    Abandoned {
        at: DateTime::from_timestamp(at, 0).unwrap(),
        reason: Reason::Overlap,
        host: Some(host.into()),
        other: Some("b".into()),
    }
}

fn far() -> Instant {
    Instant::now() + LOCK_WAIT
}

#[test]
fn a_notice_is_shown_once_then_gone() {
    let home = tempfile::tempdir().unwrap();
    setup(home.path(), "a");
    assert!(record(home.path(), "a", overlap(1, "idp.example"), far()).unwrap());
    let first = take_notices(home.path(), &["a".into()]);
    assert_eq!(first.len(), 1);
    assert!(
        first[0].contains("idp.example") && first[0].contains("\"b\""),
        "{}",
        first[0]
    );
    assert!(
        first[0].contains("Log in again for this agent"),
        "names the recovery"
    );
    assert!(take_notices(home.path(), &["a".into()]).is_empty());
}

#[test]
fn a_newer_abandon_recorded_while_shown_survives_the_clear() {
    let home = tempfile::tempdir().unwrap();
    setup(home.path(), "a");
    record(home.path(), "a", overlap(1, "old.example"), far()).unwrap();
    let shown = pending(home.path(), "a").unwrap().unwrap();
    // Recorded while the first notice was on screen.
    record(home.path(), "a", overlap(2, "new.example"), far()).unwrap();
    assert!(!clear_shown(home.path(), "a", &shown).unwrap());
    let left = pending(home.path(), "a").unwrap().unwrap();
    assert_eq!(left.host.as_deref(), Some("new.example"));
}

#[test]
fn a_held_lock_times_out_and_a_dropped_one_frees_the_next_writer() {
    let home = tempfile::tempdir().unwrap();
    setup(home.path(), "a");
    let held = lock(home.path(), "a", far()).unwrap().expect("first lock");
    let soon = Instant::now() + Duration::from_millis(100);
    assert!(lock(home.path(), "a", soon).unwrap().is_none());
    drop(held);
    assert!(lock(home.path(), "a", far()).unwrap().is_some());
}

#[test]
fn a_sigkilled_holder_does_not_block_the_next_writer() {
    // An OS advisory lock dies with its process: a child takes it, is
    // SIGKILLed, and the next writer gets it at once.
    let home = tempfile::tempdir().unwrap();
    setup(home.path(), "a");
    let lock_path = paths::profile_dir(home.path(), "a").join(LOCK_FILE);
    std::fs::write(&lock_path, b"").unwrap();
    // The holder IS the killed process (no shell in between that would
    // leave a child holding the descriptor).
    let code = format!(
        "import fcntl,time; f=open({p:?},'a'); fcntl.flock(f, fcntl.LOCK_EX); print(1, flush=True); time.sleep(60)",
        p = lock_path.display().to_string()
    );
    let mut child = std::process::Command::new("python3")
        .args(["-c", &code])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = [0u8; 1];
    std::io::Read::read_exact(child.stdout.as_mut().unwrap(), &mut ready).unwrap();
    assert!(
        lock(
            home.path(),
            "a",
            Instant::now() + Duration::from_millis(100)
        )
        .unwrap()
        .is_none()
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(lock(home.path(), "a", far()).unwrap().is_some());
}

#[test]
fn notices_carry_no_value_only_host_and_profile_names() {
    for reason in [Reason::Overlap, Reason::NewerCopy, Reason::LockTimeout] {
        let a = Abandoned {
            at: DateTime::from_timestamp(1, 0).unwrap(),
            reason,
            host: Some("idp.example".into()),
            other: Some("b".into()),
        };
        let n = a.notice("a");
        assert!(n.starts_with("Browser profile \"a\""), "{n}");
    }
}
