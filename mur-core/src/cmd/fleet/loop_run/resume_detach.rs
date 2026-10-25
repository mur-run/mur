//! `mur fleet resume <name> --detach` (pause/resume design §4).
//!
//! An operator-spawned child, never a daemon path: this process re-executes
//! `mur fleet resume <name>` in a new session (`setsid`), with stdin closed and
//! output appended to `~/.mur/runs/<run_id>/resume.log`, and returns once the
//! child holds the fleet's run lock. The daemon has no task queue, and
//! enqueueing there would turn an attended resume into an unattended run.
//!
//! stdin is null, so the child resolves unattended: an unanswered HITL gate
//! defers (`resume_from` tightens to `Defer`), it never waits on a terminal
//! nobody is watching.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::{resume, run_lock};

/// The log a detached resume appends to, beside the run's `run.json`.
pub const RESUME_LOG_FILE: &str = "resume.log";

/// How long `--detach` waits for the child to take the run lock. Admission and
/// the lock come before any model call, so a healthy child holds it well
/// inside this; elapsing it means "not confirmed yet", never "failed".
const CONFIRM_WINDOW: Duration = Duration::from_secs(8);
const CONFIRM_POLL: Duration = Duration::from_millis(200);
/// Bytes of `resume.log` shown when the child dies during startup.
const LOG_TAIL_BYTES: u64 = 4096;

/// Win32 process-creation flags (`winbase.h`): no console is inherited or
/// created, and the child gets its own process group, so neither a closing
/// console window nor Ctrl+C in the parent's ends the resumed run.
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `<mur_home>/runs/<run_id>/resume.log`.
pub fn resume_log_path(mur_home: &Path, run_id: &str) -> PathBuf {
    crate::run_status::store::runs_dir(mur_home)
        .join(run_id)
        .join(RESUME_LOG_FILE)
}

/// The child command: this same binary, `fleet resume <name>` without
/// `--detach`, pinned to the same MUR home, stdin closed, output to `log`.
fn child_command(exe: &Path, mur_home: &Path, name: &str, log: &std::fs::File) -> Result<Command> {
    let mut cmd = Command::new(exe);
    cmd.args(["fleet", "resume", name])
        .env(mur_common::home::MUR_HOME_ENV, mur_home)
        .stdin(Stdio::null())
        .stdout(log.try_clone().context("duplicate the resume log handle")?)
        .stderr(log.try_clone().context("duplicate the resume log handle")?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid(2) is async-signal-safe and touches no memory of the
        // parent; it only makes the child a session leader, so closing this
        // shell (SIGHUP to its session) does not end the resumed run.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    Ok(cmd)
}

/// Hand a still-running child to a thread that waits on it, so a long-lived
/// caller (the Hub, murmur) reaps it when the run ends instead of keeping a
/// zombie until it exits. In the CLI the process exits first and the child is
/// reparented as usual; the thread simply dies with it.
fn reap_in_background(mut child: Child) {
    let spawned = std::thread::Builder::new()
        .name("mur-resume-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    if let Err(e) = spawned {
        tracing::debug!("could not start the resume reaper thread: {e}");
    }
}

/// What the confirm window saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Startup {
    /// OUR child holds the run lock: the run is live.
    Running,
    /// The child exited inside the window.
    Exited(ExitStatus),
    /// Neither, yet.
    Unconfirmed,
}

/// Watch the spawned child until it takes the run lock, exits, or `window`
/// elapses. The lock must be held by THIS child's pid: two resumes can pass
/// admission together, and the loser's child must not borrow the winner's lock
/// as its own start — it exits on the refused lock and reads as `Exited`.
fn watch_startup(child: &mut Child, mur_home: &Path, name: &str, window: Duration) -> Startup {
    let pid = child.id();
    let deadline = Instant::now() + window;
    loop {
        // Exit first: a run that finished inside the window released the lock
        // already, and a child that died must never read as running.
        if let Ok(Some(status)) = child.try_wait() {
            return Startup::Exited(status);
        }
        if run_lock::is_held_by(mur_home, name, pid) {
            return Startup::Running;
        }
        if Instant::now() >= deadline {
            return Startup::Unconfirmed;
        }
        std::thread::sleep(CONFIRM_POLL);
    }
}

/// `mur fleet resume <name> --detach`.
pub fn cmd_fleet_resume_detached(mur_home: &Path, name: &str) -> Result<()> {
    let exe = std::env::current_exe().context("locate the mur binary")?;
    println!("{}", resume_detached(mur_home, name, &exe)?);
    Ok(())
}

/// The detached resume, with the `mur` binary to re-execute given explicitly:
/// the CLI passes itself, the Hub passes the `mur` it resolved (its own
/// executable is the Hub, not `mur`). Returns what to tell the operator; a
/// child that died during startup is an `Err` carrying the log tail.
pub fn resume_detached(mur_home: &Path, name: &str, exe: &Path) -> Result<String> {
    // Admit here so a refusal reads where the operator is, not in a log. The
    // child admits again (and again under the lock) — this check grants nothing.
    let cp = resume::admit(mur_home, name)?;
    let log_path = resume_log_path(mur_home, &cp.run_id);
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let mut child = child_command(exe, mur_home, name, &log)?
        .spawn()
        .with_context(|| format!("spawn {}", exe.display()))?;
    let pid = child.id();
    let log_shown = log_path.display();
    let run_id = &cp.run_id;
    let startup = watch_startup(&mut child, mur_home, name, CONFIRM_WINDOW);
    // An exited child was already reaped by `try_wait`; a live one is waited
    // on in the background.
    if !matches!(startup, Startup::Exited(_)) {
        reap_in_background(child);
    }
    Ok(match startup {
        Startup::Running => format!(
            "Resumed fleet '{name}' (run {run_id}) in the background, pid {pid}.\n  log:    {log_shown}\n  watch:  mur fleet status {name}\n  pause:  mur fleet pause {name}   stop: mur fleet stop {name}"
        ),
        Startup::Exited(status) if status.success() => {
            format!("Fleet '{name}' (run {run_id}) resumed and already ended — see {log_shown}")
        }
        Startup::Exited(status) => bail!(
            "the detached resume of '{name}' exited during startup ({status}).\n--- last lines of {log_shown} ---\n{}",
            crate::cmd::agent::start::tail_of(&log_path, LOG_TAIL_BYTES)
        ),
        Startup::Unconfirmed => format!(
            "Resuming fleet '{name}' (run {run_id}) in the background, pid {pid} — not confirmed within {}s.\n  Check `mur fleet status {name}` or `tail {log_shown}`.",
            CONFIRM_WINDOW.as_secs()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lives_beside_the_run_record() {
        let home = Path::new("/h");
        let log = resume_log_path(home, "run-1");
        let record = crate::run_status::store::run_path(home, "run-1");
        assert_eq!(log.parent(), record.parent());
        assert_eq!(log.file_name().unwrap(), RESUME_LOG_FILE);
    }

    #[test]
    fn child_resumes_in_the_foreground_under_the_same_home() {
        let dir = tempfile::tempdir().unwrap();
        let log = std::fs::File::create(dir.path().join("l")).unwrap();
        let cmd = child_command(Path::new("/bin/mur"), dir.path(), "dev", &log).unwrap();
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        // Never `--detach` again: the child is the foreground resume.
        assert_eq!(args, ["fleet", "resume", "dev"]);
        let home = cmd
            .get_envs()
            .find(|(k, _)| *k == mur_common::home::MUR_HOME_ENV)
            .and_then(|(_, v)| v);
        assert_eq!(home, Some(dir.path().as_os_str()));
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_dies_reads_as_exited_never_running() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new("false").spawn().unwrap();
        let got = watch_startup(&mut child, dir.path(), "dev", CONFIRM_WINDOW);
        assert!(matches!(got, Startup::Exited(s) if !s.success()), "{got:?}");
    }

    /// A child left running is reaped once it ends: `kill(pid, 0)` still
    /// succeeds on a zombie, so ESRCH proves the background wait collected it.
    #[cfg(unix)]
    #[test]
    fn a_live_child_is_reaped_when_it_ends() {
        let child = Command::new("sleep").arg("0.2").spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        reap_in_background(child);
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only probes for existence; nothing is delivered.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(Instant::now() < deadline, "pid {pid} was never reaped");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Two resumes raced: another process holds the fleet's lock while our
    /// child is still alive. That lock is not our child's start.
    #[cfg(unix)]
    #[test]
    fn another_holders_lock_never_confirms_our_child() {
        let dir = tempfile::tempdir().unwrap();
        let _winner = run_lock::acquire(dir.path(), "dev").unwrap();
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        let got = watch_startup(&mut child, dir.path(), "dev", Duration::from_millis(600));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(got, Startup::Unconfirmed);
    }
}
