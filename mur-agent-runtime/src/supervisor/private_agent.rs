//! The runtime-private ssh-agent that holds the agent's dedicated key (D spec
//! §"Private agent lifecycle", plan Task 16.2).
//!
//! Started before the seal, while `secrets/` is readable: a fresh
//! `run-<pid>/` (0700) under the key directory, OpenSSH's own `ssh-agent` on a
//! socket inside it, the key added with `ssh-add`, one held connection, then
//! the socket and its directory are removed. The held connection stays valid;
//! nothing can open another, sealed or not.
//!
//! Lifetime (plan 16.9): `ssh-agent -D` outlives a parent killed with
//! SIGKILL. So a small shell starts `ssh-agent -D` and also waits on a pipe
//! only this runtime holds; when the pipe closes — the handle dropped, or the
//! runtime gone however it went — the shell kills the agent. The spawned
//! child is that shell, so a dead agent is seen at once as the shell's exit.
//! No pid file, no orphan sweep.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use mur_common::hitl::approval_token::create_private_dir;
use mur_ssh_request::keystore::{KeystoreError, StoredKey};
use mur_ssh_request::{PRIVATE_AGENT_SOCKET_WAIT, SSH_AGENT_BIN};
use tokio::process::Child;

use crate::ssh_proxy::listener::{self, DedicatedUpstream, HeldUpstream};

/// Prefix of the per-start directory under the agent's key directory.
pub(super) const RUN_DIR_PREFIX: &str = "run-";
/// The private agent's socket inside the run directory.
pub(super) const SOCKET_NAME: &str = "agent.sock";
/// Runs `$2 -D -a $1` and kills it at end of stdin, which is when the
/// runtime holding the other end of the pipe drops it or is gone. The shell
/// exits with the agent, so an agent that dies is noticed at once.
const LIFELINE_SHELL: &str = "/bin/sh";
const LIFELINE_SCRIPT: &str = r#"exec 3<&0
"$2" -D -a "$1" </dev/null >/dev/null 2>&1 3<&- &
a=$!
(read _ <&3; kill "$a" 2>/dev/null) &
wait "$a""#;
/// `$0` of the lifeline shell, shown by `ps`.
const LIFELINE_NAME: &str = "mur-ssh-agent-lifeline";
const SOCKET_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, thiserror::Error)]
pub(super) enum StartError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the private ssh-agent socket path is too long for a unix socket")]
    PathTooLong,
    #[error("ssh-agent did not open its socket within {0:?}")]
    NoSocket(Duration),
    #[error("could not add the key to the private ssh-agent: {0}")]
    Load(#[from] KeystoreError),
    #[error("the private ssh-agent did not list its keys")]
    NoIdentities,
    #[error("ssh-agent exited before opening its socket ({0})")]
    Exited(std::process::ExitStatus),
}

/// Start the private agent for `key`, whose directory is `key_dir`.
pub(super) async fn start(
    key_dir: &Path,
    key: &StoredKey,
) -> Result<DedicatedUpstream, StartError> {
    start_with(
        key_dir,
        key,
        spawn_agent,
        mur_ssh_request::keystore::load_into,
    )
    .await
}

/// [`start`] with the spawner and loader given, so tests need no ssh-agent.
pub(super) async fn start_with<S, L>(
    key_dir: &Path,
    key: &StoredKey,
    spawn: S,
    load: L,
) -> Result<DedicatedUpstream, StartError>
where
    S: FnOnce(&Path) -> std::io::Result<Child>,
    L: FnOnce(&StoredKey, &Path) -> Result<(), KeystoreError> + Send + 'static,
{
    sweep_runs(key_dir);
    let run = run_dir(key_dir);
    create_private_dir(&run)?;
    let started = start_in(&run, key, spawn, load).await;
    // Step 6, on success and failure alike: the socket and its directory go.
    let _ = std::fs::remove_dir_all(&run);
    started
}

async fn start_in<S, L>(
    run: &Path,
    key: &StoredKey,
    spawn: S,
    load: L,
) -> Result<DedicatedUpstream, StartError>
where
    S: FnOnce(&Path) -> std::io::Result<Child>,
    L: FnOnce(&StoredKey, &Path) -> Result<(), KeystoreError> + Send + 'static,
{
    let sock = run.join(SOCKET_NAME);
    if !listener::socket_path_fits(&sock) {
        return Err(StartError::PathTooLong);
    }
    // `kill_on_drop`: every early return below stops it.
    let mut child = spawn(&sock)?;
    wait_for_socket(&sock, &mut child).await?;
    let (k, s) = (key.clone(), sock.clone());
    tokio::task::spawn_blocking(move || load(&k, &s))
        .await
        .map_err(std::io::Error::other)??;
    let held = HeldUpstream::connect(&sock).await?;
    let blobs: HashSet<Vec<u8>> = listener::identity_blobs(&held)
        .await
        .ok_or(StartError::NoIdentities)?;
    Ok(DedicatedUpstream::new(held, blobs, Some(child)))
}

/// `<key_dir>/run-<pid>/`.
pub(super) fn run_dir(key_dir: &Path) -> PathBuf {
    key_dir.join(format!("{RUN_DIR_PREFIX}{}", std::process::id()))
}

/// The lifeline shell running `ssh-agent -D -a <sock>`, stdin a pipe this
/// process holds (close-on-exec, so no child of the runtime keeps it open).
pub(super) fn spawn_agent(sock: &Path) -> std::io::Result<Child> {
    tokio::process::Command::new(LIFELINE_SHELL)
        .args(["-c", LIFELINE_SCRIPT, LIFELINE_NAME])
        .arg(sock)
        .arg(SSH_AGENT_BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
}

/// Wait for the socket; give up at once if `ssh-agent` has already exited.
async fn wait_for_socket(sock: &Path, child: &mut Child) -> Result<(), StartError> {
    let deadline = Instant::now() + PRIVATE_AGENT_SOCKET_WAIT;
    while !sock.exists() {
        if let Some(status) = child.try_wait()? {
            return Err(StartError::Exited(status));
        }
        if Instant::now() >= deadline {
            return Err(StartError::NoSocket(PRIVATE_AGENT_SOCKET_WAIT));
        }
        tokio::time::sleep(SOCKET_POLL).await;
    }
    Ok(())
}

/// Remove run directories a runtime killed mid-start left behind. A link is
/// removed, never followed. `run-<pid>` of a process still alive is kept:
/// another start of this agent (a restart overlapping the old runtime) may
/// be using it right now.
pub(super) fn sweep_runs(key_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(key_dir) else {
        return;
    };
    // Our own pid's directory can only be one a dead process left (pid reuse).
    let own = std::process::id().to_string();
    for e in entries.flatten() {
        let is_run = e
            .file_name()
            .to_str()
            .and_then(|n| n.strip_prefix(RUN_DIR_PREFIX))
            .is_some_and(|pid| pid == own || !owner_alive(pid));
        match e.metadata() {
            Ok(m) if is_run && m.is_dir() => {
                let _ = std::fs::remove_dir_all(e.path());
            }
            Ok(m) if is_run && m.is_symlink() => {
                let _ = std::fs::remove_file(e.path());
            }
            _ => {}
        }
    }
}

/// Whether the `<pid>` suffix of a run directory names a live process.
/// Unparsable suffixes are not a live owner; `EPERM` means it exists.
pub(super) fn owner_alive(pid: &str) -> bool {
    let Ok(pid) = pid.parse::<libc::pid_t>() else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill(2) with signal 0 only checks that the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
#[path = "private_agent_tests.rs"]
mod tests;
