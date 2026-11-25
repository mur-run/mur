use super::*;
use std::sync::mpsc;
use std::time::Duration;

const RELEASED_WITHIN: Duration = Duration::from_secs(5);

#[test]
fn the_release_check_script_looks_for_the_marker_this_crate_embeds() {
    let script = include_str!("../../../scripts/check-no-test-pause.sh");
    let marker = std::str::from_utf8(BUILD_MARKER).unwrap();
    assert!(script.contains(marker), "script and BUILD_MARKER drifted");
}

#[cfg(not(feature = "test-pause"))]
mod without_the_feature {
    use super::*;
    use crate::test_env::EnvGuard;

    #[test]
    fn pause_point_never_reads_the_environment_or_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gate"), b"").unwrap();
        let _env = EnvGuard::set([(PAUSE_DIR_ENV, dir.path())]);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            pause_point("gate");
            tx.send(()).unwrap();
        });
        rx.recv_timeout(RELEASED_WITHIN)
            .expect("a shipped build must not block on a pause file");
    }
}

#[cfg(feature = "test-pause")]
mod with_the_feature {
    use super::*;
    use crate::test_env::EnvGuard;

    /// Long enough that a blocked call would still be blocked, short enough to
    /// keep the suite quick.
    const BLOCKED_FOR: Duration = Duration::from_millis(250);

    #[test]
    fn the_embedded_marker_is_the_published_marker() {
        assert_eq!(&EMBEDDED_MARKER[..], BUILD_MARKER);
    }

    #[test]
    fn pause_point_blocks_while_the_file_exists_and_returns_when_it_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let gate = dir.path().join("gate");
        std::fs::write(&gate, b"").unwrap();
        let _env = EnvGuard::set([(PAUSE_DIR_ENV, dir.path())]);
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            pause_point("gate");
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(BLOCKED_FOR).is_err(),
            "returned while the file existed"
        );
        std::fs::remove_file(&gate).unwrap();
        rx.recv_timeout(RELEASED_WITHIN)
            .expect("still blocked after the file was removed");
        worker.join().unwrap();
    }

    #[test]
    fn pause_point_does_not_block_without_the_file_or_the_variable() {
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set([(PAUSE_DIR_ENV, dir.path())]);
        pause_point("no-such-gate");
        drop(_env);
        let _unset = EnvGuard::unset([PAUSE_DIR_ENV]);
        pause_point("gate");
    }

    #[test]
    fn only_that_name_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("other"), b"").unwrap();
        let _env = EnvGuard::set([(PAUSE_DIR_ENV, dir.path())]);
        pause_point("gate");
    }

    #[test]
    fn a_name_that_could_leave_the_directory_is_refused() {
        for bad in ["", "..", "a/b", "../x", ".hidden"] {
            assert!(!valid_pause_name(bad), "{bad:?}");
        }
        assert!(valid_pause_name("before-create_reply.dir"));
    }
}
