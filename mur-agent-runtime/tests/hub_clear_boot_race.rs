//! Two real runtime processes against the Hub's `clear_runtime_state` (#790).
//!
//! A booting supervisor takes the `running.sentinel` flock before it seals the
//! startup override, and only writes `running.lock` after. If the Hub clears
//! the agent's files inside that window, it unlinks the sentinel A still holds
//! a flock on, and a second runtime B gets a fresh inode with an uncontested
//! flock: two supervisors on one agent home.
//!
//! The window is held open deterministically: the live override is a FIFO, so
//! A claims it (rename to `.consumed.json`) and then blocks in `read` until the
//! test writes to it. The Hub side is the public `Supervisor::start`, which is
//! exactly the production path — `ensure_running → spawn_runtime →
//! clear_runtime_state → spawn`.
//!
//! Unix-only: it needs `mkfifo` and flock semantics.
#![cfg(unix)]

use fs2::FileExt;
use mur_common::startup_override::claim;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const AGENT: &str = "agent_t";
/// Bounds a hang only; a cold boot reaches the flock in well under a second.
/// Generous because nextest runs the workspace in parallel (see
/// `supervisor_startup.rs`).
const BOOT_BUDGET: Duration = Duration::from_secs(60);
/// How long B gets to be refused once spawned.
const REFUSAL_BUDGET: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// What `lock_file::acquire` reports when the flock is already held.
const REFUSAL_MARKER: &str = "already running";

/// Kills the child on drop, so a failed assertion never orphans a runtime
/// (an orphan keeps inherited handles open and nextest reports LEAK).
struct Reaped(Child);
impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_until(budget: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < budget {
        if done() {
            return true;
        }
        std::thread::sleep(POLL);
    }
    done()
}

/// Whether someone else holds the flock on `path`.
fn flocked(path: &Path) -> bool {
    let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    else {
        return false;
    };
    match f.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&f);
            false
        }
        Err(_) => true,
    }
}

fn lock_pid(agent_home: &Path) -> Option<u32> {
    let s = std::fs::read_to_string(agent_home.join("running.lock")).ok()?;
    serde_json::from_str::<serde_json::Value>(&s).ok()?["pid"]
        .as_u64()
        .map(|p| p as u32)
}

fn mkfifo(path: &Path) {
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
    let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
}

fn seed_home(home: &Path) {
    let agent_home = home.join("agents").join(AGENT);
    std::fs::create_dir_all(&agent_home).unwrap();
    // B must be stopped by the lock, not by the B1 sandbox refusing to start
    // unconfined on a platform where it cannot enforce — otherwise the sandbox
    // masks the bug and the test passes against the broken clear.
    let profile = include_str!("fixtures/profile_stdio.yaml").replace(
        "  syscalls: { mode: default }",
        "  syscalls: { mode: default }\n  fail_closed_on_sandbox_error: false",
    );
    assert!(profile.contains("fail_closed_on_sandbox_error: false"));
    std::fs::write(agent_home.join("profile.yaml"), profile).unwrap();
    std::fs::write(agent_home.join("sys_prompt.md"), "You are a test.").unwrap();
    let dir = claim::overrides_dir(home);
    std::fs::create_dir_all(&dir).unwrap();
    mkfifo(&claim::live_path(&dir, AGENT));
}

#[tokio::test(flavor = "multi_thread")]
async fn hub_start_during_boot_window_does_not_admit_a_second_runtime() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let agent_home = home.join("agents").join(AGENT);
    let sentinel = agent_home.join("running.sentinel");
    seed_home(home);

    let bin = env!("CARGO_BIN_EXE_mur-agent-runtime");
    // The Hub's `find_runtime_binary` reads this; the guard restores it.
    let mut env = mur_common::test_env::EnvGuard::hold();
    env.set_var("MUR_AGENT_RUNTIME_BIN", bin);

    // A: boots, takes the sentinel flock, claims the FIFO and blocks reading it.
    let mut a = Reaped(
        Command::new(bin)
            .env("MUR_HOME", home)
            .args(["--profile", AGENT, "start"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let consumed = claim::consumed_path(&claim::overrides_dir(home), AGENT);
    assert!(
        wait_until(BOOT_BUDGET, || flocked(&sentinel) && consumed.exists()),
        "A never reached the override read with the sentinel flocked"
    );
    assert!(
        a.0.try_wait().unwrap().is_none(),
        "A exited before the window"
    );
    assert!(
        !agent_home.join("running.lock").exists(),
        "precondition: A is inside the window, so running.lock is not written yet"
    );

    // The Hub starts the agent now: it sees no running.lock, clears the
    // agent's files and spawns B.
    let hub = mur_gui_core::sidecar::Supervisor::new(home.to_path_buf());
    hub.start(AGENT).await.expect("hub spawned B");

    assert!(
        sentinel.exists(),
        "clear_runtime_state unlinked the sentinel A holds a flock on"
    );
    let stderr_log = agent_home.join("stderr.log");
    assert!(
        wait_until(REFUSAL_BUDGET, || std::fs::read_to_string(&stderr_log)
            .is_ok_and(|s| s.contains(REFUSAL_MARKER))),
        "B was not refused by the lock; stderr.log:\n{}",
        std::fs::read_to_string(&stderr_log).unwrap_or_default()
    );

    // Release A: it finishes booting and must be the only owner.
    {
        use std::io::Write;
        let mut w = std::fs::OpenOptions::new()
            .write(true)
            .open(&consumed)
            .unwrap();
        w.write_all(b"{}").unwrap();
    }
    let a_pid = a.0.id();
    assert!(
        wait_until(BOOT_BUDGET, || lock_pid(&agent_home) == Some(a_pid)),
        "running.lock should name A ({a_pid}) after it resumes, found {:?}",
        lock_pid(&agent_home)
    );
    assert!(a.0.try_wait().unwrap().is_none(), "A died after resuming");

    // Kills B if it is somehow still alive; A is reaped by its guard.
    hub.shutdown().await;
}
