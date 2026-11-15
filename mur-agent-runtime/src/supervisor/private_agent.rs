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
//! SIGKILL. So `ssh-agent` runs in command mode with a shell that waits on a
//! pipe only this runtime holds. When the runtime exits, however it exits,
//! the pipe closes, the shell exits, and `ssh-agent` follows it (it checks
//! its command about every ten seconds). No pid file, no orphan sweep.

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
/// The command `ssh-agent` runs and watches: it exits at end of input,
/// which is when the runtime holding the other end of the pipe is gone.
const LIFELINE_SHELL: &str = "/bin/sh";
const LIFELINE_SCRIPT: &str = "read _";
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
    let child = spawn(&sock)?;
    wait_for_socket(&sock).await?;
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

/// `ssh-agent -a <sock> -- /bin/sh -c 'read _'`, stdin a pipe this process
/// holds (close-on-exec, so no child of the runtime keeps it open).
pub(super) fn spawn_agent(sock: &Path) -> std::io::Result<Child> {
    tokio::process::Command::new(SSH_AGENT_BIN)
        .arg("-a")
        .arg(sock)
        .arg("--")
        .args([LIFELINE_SHELL, "-c", LIFELINE_SCRIPT])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
}

async fn wait_for_socket(sock: &Path) -> Result<(), StartError> {
    let deadline = Instant::now() + PRIVATE_AGENT_SOCKET_WAIT;
    while !sock.exists() {
        if Instant::now() >= deadline {
            return Err(StartError::NoSocket(PRIVATE_AGENT_SOCKET_WAIT));
        }
        tokio::time::sleep(SOCKET_POLL).await;
    }
    Ok(())
}

/// Remove run directories a runtime killed mid-start left behind. A link is
/// removed, never followed.
fn sweep_runs(key_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(key_dir) else {
        return;
    };
    for e in entries.flatten() {
        let is_run = e
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(RUN_DIR_PREFIX));
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

#[cfg(test)]
#[path = "private_agent_tests.rs"]
mod tests;
