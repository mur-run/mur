//! Start an agent that `mur agent cli` / `murmur` was asked to open but is not
//! running (#1767). Opening a chat is an explicit request to talk to the
//! agent, so asking the user to run `mur agent start` first and retry is a
//! detour, most often hit right after a reboot or a MUR restart when the
//! concierge has no service unit to bring it back.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;

/// How long to wait, after `mur agent start` returns, for the runtime to write
/// `running.lock`. `start` already waits a short confirm window; a heavy start
/// (local model load) can legitimately take longer, and the chat needs the
/// socket, so wait a while more before giving up with the manual hint.
const READY_WINDOW: Duration = Duration::from_secs(60);
const READY_POLL: Duration = Duration::from_millis(250);

/// Ensure `agent` is running. Returns `Ok(true)` once its `running.lock`
/// exists, `Ok(false)` when it could not be confirmed (the caller prints the
/// manual hint), and an error only when `start` itself refused.
pub(super) fn ensure_running(home: &Path, agent: &str) -> Result<bool> {
    let lock = home.join("agents").join(agent).join("running.lock");
    if lock.exists() {
        return Ok(true);
    }
    eprintln!("Agent '{agent}' is not running — starting it…");
    crate::cmd::agent::start::cmd_start(agent)?;
    Ok(wait_for(&lock, READY_WINDOW, READY_POLL))
}

/// Poll until `path` exists or `window` elapses.
fn wait_for(path: &Path, window: Duration, poll: Duration) -> bool {
    let deadline = Instant::now() + window;
    loop {
        if path.exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_for_sees_a_file_that_appears_later() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("running.lock");
        let p = path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(p, b"{}").unwrap();
        });
        assert!(wait_for(
            &path,
            Duration::from_secs(5),
            Duration::from_millis(10)
        ));
        writer.join().unwrap();
    }

    #[test]
    fn wait_for_gives_up_after_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("running.lock");
        assert!(!wait_for(
            &path,
            Duration::from_millis(50),
            Duration::from_millis(10)
        ));
    }

    #[test]
    fn ensure_running_is_a_no_op_when_the_lock_exists() {
        let home = tempfile::tempdir().unwrap();
        let agent_dir = home.path().join("agents").join("mur");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("running.lock"), b"{}").unwrap();
        assert!(ensure_running(home.path(), "mur").unwrap());
    }
}
