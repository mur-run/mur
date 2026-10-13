//! Spawn helpers that ride out `ETXTBSY` ("Text file busy").
//!
//! `execve` of a file that any process still holds open for writing fails
//! with `ETXTBSY`. A thread that has just written an executable and closed it
//! can still lose this race: another thread's `fork()` copies the write fd
//! into its child, and the copy only goes away at the child's own `exec`.
//! The window is microseconds, but it is real both for an installer that has
//! just replaced a binary and for tests that write a fake one, and every
//! caller recovers the same way: wait a beat and try again.

use std::io;
use std::time::Duration;

/// Attempts after the first. The window is a fork-to-exec gap, so a handful
/// of short waits is far more than it ever needs.
const MAX_RETRIES: u32 = 0; // CONTROL: retries disabled
/// Linear backoff step; the total wait across all retries stays under 300 ms.
const BACKOFF_STEP: Duration = Duration::from_millis(8);

fn is_busy(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::ExecutableFileBusy
}

/// `cmd.spawn()`, retried while the target is busy. Any other error, and the
/// busy error once the retries run out, is returned unchanged.
pub(crate) async fn spawn(cmd: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    let mut attempt = 0;
    loop {
        match cmd.spawn() {
            Err(e) if is_busy(&e) && attempt < MAX_RETRIES => {
                attempt += 1;
                tokio::time::sleep(BACKOFF_STEP * attempt).await;
            }
            other => return other,
        }
    }
}

/// `cmd.output()`, retried while the target is busy.
pub(crate) fn output(cmd: &mut std::process::Command) -> io::Result<std::process::Output> {
    let mut attempt = 0;
    loop {
        match cmd.output() {
            Err(e) if is_busy(&e) && attempt < MAX_RETRIES => {
                attempt += 1;
                std::thread::sleep(BACKOFF_STEP * attempt);
            }
            other => return other,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn other_errors_are_not_retried() {
        let started = std::time::Instant::now();
        let err = spawn(&mut tokio::process::Command::new("/no/such/murtest-binary"))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(
            started.elapsed() < BACKOFF_STEP,
            "NotFound must fail at once"
        );
    }
}

/// Linux only: macOS and the BSDs let `execve` run a file that is still open
/// for writing, so there is no `ETXTBSY` to provoke and these would pass
/// without exercising the retry.
#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A script that is currently open for writing, exactly the state a
    /// concurrent fork leaves behind. Dropping the returned file ends it.
    fn script_held_open(dir: &tempfile::TempDir) -> (std::path::PathBuf, std::fs::File) {
        let bin = dir.path().join("fake");
        let mut f = std::fs::File::create(&bin).unwrap();
        f.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (bin, f)
    }

    #[test]
    fn a_held_open_script_is_busy_without_the_helper() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, _writer) = script_held_open(&dir);
        let err = std::process::Command::new(&bin).output().unwrap_err();
        assert!(is_busy(&err), "control: expected ETXTBSY, got {err:?}");
    }

    #[test]
    fn output_waits_out_a_writer_that_closes_soon() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, writer) = script_held_open(&dir);
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(writer);
        });
        let out = output(&mut std::process::Command::new(&bin)).unwrap();
        assert!(out.status.success());
        closer.join().unwrap();
    }

    #[tokio::test]
    async fn spawn_waits_out_a_writer_that_closes_soon() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, writer) = script_held_open(&dir);
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(writer);
        });
        let mut child = spawn(&mut tokio::process::Command::new(&bin))
            .await
            .unwrap();
        assert!(child.wait().await.unwrap().success());
        closer.join().unwrap();
    }
}
