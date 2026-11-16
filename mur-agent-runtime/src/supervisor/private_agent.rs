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
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ssh_proxy::listener::{self, DedicatedUpstream, HeldUpstream, UPSTREAM_IO_TIMEOUT};
use crate::ssh_proxy::wire;
use mur_common::hitl::approval_token::create_private_dir;
use mur_ssh_request::keystore::{KeystoreError, StoredKey};
use mur_ssh_request::{PRIVATE_AGENT_SOCKET_WAIT, SSH_AGENT_BIN};

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

/// The private agent as the pre-seal phase leaves it: a plain std stream,
/// because the runtime that will drive it does not exist yet (see
/// `supervisor::seal`). [`Self::adopt`] hands it to tokio after the seal.
pub(crate) struct PreSealDedicated {
    stream: std::os::unix::net::UnixStream,
    blobs: HashSet<Vec<u8>>,
    lifeline: Option<Lifeline>,
}

impl PreSealDedicated {
    /// Must run inside the runtime. `None` (warned) when the stream cannot
    /// be registered: requests for the key are then refused.
    pub(crate) fn adopt(self) -> Option<Arc<DedicatedUpstream>> {
        match HeldUpstream::from_std(self.stream) {
            Ok(held) => Some(Arc::new(DedicatedUpstream::new(
                held,
                self.blobs,
                self.lifeline
                    .map(|l| Box::new(l) as Box<dyn std::any::Any + Send + Sync>),
            ))),
            Err(e) => {
                tracing::warn!(error = %e, "ssh-agent proxy: could not adopt the private ssh-agent connection; requests for the dedicated key will be refused");
                None
            }
        }
    }

    #[cfg(test)]
    pub(super) fn blobs(&self) -> &HashSet<Vec<u8>> {
        &self.blobs
    }

    /// A handle with no agent behind it, for boot tests that count calls.
    #[cfg(test)]
    pub(super) fn detached() -> Self {
        let (stream, _) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        Self {
            stream,
            blobs: HashSet::new(),
            lifeline: None,
        }
    }
}

/// The lifeline shell. Dropping it closes the pipe, so the shell kills
/// `ssh-agent`; the shell itself is then killed and reaped.
pub(super) struct Lifeline(Child);

impl Drop for Lifeline {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start the private agent for `key`, whose directory is `key_dir`.
/// Blocking on purpose: it runs before the tokio runtime exists, on the
/// thread that is about to seal.
pub(super) fn start(key_dir: &Path, key: &StoredKey) -> Result<PreSealDedicated, StartError> {
    start_with(
        key_dir,
        key,
        spawn_agent,
        mur_ssh_request::keystore::load_into,
    )
}

/// [`start`] with the spawner and loader given, so tests need no ssh-agent.
pub(super) fn start_with<S, L>(
    key_dir: &Path,
    key: &StoredKey,
    spawn: S,
    load: L,
) -> Result<PreSealDedicated, StartError>
where
    S: FnOnce(&Path) -> std::io::Result<Child>,
    L: FnOnce(&StoredKey, &Path) -> Result<(), KeystoreError>,
{
    sweep_runs(key_dir);
    let run = run_dir(key_dir);
    create_private_dir(&run)?;
    let started = start_in(&run, key, spawn, load);
    // Step 6, on success and failure alike: the socket and its directory go.
    let _ = std::fs::remove_dir_all(&run);
    started
}

fn start_in<S, L>(
    run: &Path,
    key: &StoredKey,
    spawn: S,
    load: L,
) -> Result<PreSealDedicated, StartError>
where
    S: FnOnce(&Path) -> std::io::Result<Child>,
    L: FnOnce(&StoredKey, &Path) -> Result<(), KeystoreError>,
{
    let sock = run.join(SOCKET_NAME);
    if !listener::socket_path_fits(&sock) {
        return Err(StartError::PathTooLong);
    }
    // Every early return below drops the lifeline, which stops the agent.
    let mut lifeline = Lifeline(spawn(&sock)?);
    wait_for_socket(&sock, &mut lifeline.0)?;
    load(key, &sock)?;
    let mut stream = HeldUpstream::connect_blocking(&sock)?;
    let blobs = identity_blobs_blocking(&mut stream)?.ok_or(StartError::NoIdentities)?;
    Ok(PreSealDedicated {
        stream,
        blobs,
        lifeline: Some(lifeline),
    })
}

/// One `REQUEST_IDENTITIES` round trip on a std stream, bounded by
/// [`UPSTREAM_IO_TIMEOUT`]. `None` when the answer is not a key list.
fn identity_blobs_blocking(
    s: &mut std::os::unix::net::UnixStream,
) -> std::io::Result<Option<HashSet<Vec<u8>>>> {
    use std::io::{Read, Write};
    s.set_read_timeout(Some(UPSTREAM_IO_TIMEOUT))?;
    s.set_write_timeout(Some(UPSTREAM_IO_TIMEOUT))?;
    s.write_all(&wire::Request::RequestIdentities.encode())?;
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let Ok(n) = wire::check_len(u32::from_be_bytes(len)) else {
        return Ok(None);
    };
    let mut body = vec![0u8; n];
    s.read_exact(&mut body)?;
    // Back to no timeouts: tokio sets the stream non-blocking on adoption.
    s.set_read_timeout(None)?;
    s.set_write_timeout(None)?;
    Ok(match wire::decode_answer(&body) {
        Ok(wire::Answer::Identities(ids)) => Some(ids.into_iter().map(|i| i.key_blob).collect()),
        _ => None,
    })
}

/// `<key_dir>/run-<pid>/`.
pub(super) fn run_dir(key_dir: &Path) -> PathBuf {
    key_dir.join(format!("{RUN_DIR_PREFIX}{}", std::process::id()))
}

/// The lifeline shell running `ssh-agent -D -a <sock>`, stdin a pipe this
/// process holds (close-on-exec, so no child of the runtime keeps it open).
pub(super) fn spawn_agent(sock: &Path) -> std::io::Result<Child> {
    std::process::Command::new(LIFELINE_SHELL)
        .args(["-c", LIFELINE_SCRIPT, LIFELINE_NAME])
        .arg(sock)
        .arg(SSH_AGENT_BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Wait for the socket; give up at once if `ssh-agent` has already exited.
pub(super) fn wait_for_socket(sock: &Path, child: &mut Child) -> Result<(), StartError> {
    let deadline = Instant::now() + PRIVATE_AGENT_SOCKET_WAIT;
    while !sock.exists() {
        if let Some(status) = child.try_wait()? {
            return Err(StartError::Exited(status));
        }
        if Instant::now() >= deadline {
            return Err(StartError::NoSocket(PRIVATE_AGENT_SOCKET_WAIT));
        }
        std::thread::sleep(SOCKET_POLL);
    }
    Ok(())
}

/// Remove run directories a runtime killed mid-start left behind.
/// `run-<pid>` of a process still alive is kept: another start of this agent
/// (a restart overlapping the old runtime) may be using it right now. A link
/// is never a run directory (a start makes a real one), so it is always
/// removed, whatever its name, and never followed.
pub(super) fn sweep_runs(key_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(key_dir) else {
        return;
    };
    // Our own pid's directory can only be one a dead process left (pid reuse).
    let own = std::process::id().to_string();
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix(RUN_DIR_PREFIX)) else {
            continue;
        };
        // `DirEntry::metadata` does not follow a link.
        match e.metadata() {
            Ok(m) if m.is_symlink() => {
                let _ = std::fs::remove_file(e.path());
            }
            Ok(m) if m.is_dir() && (pid == own || !owner_alive(pid)) => {
                let _ = std::fs::remove_dir_all(e.path());
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
