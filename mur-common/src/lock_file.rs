//! Canonical `running.lock` reader + 3-state agent status classifier.
//!
//! Used by:
//! - `mur agent list/status` (CLI) — see `mur-core/src/cmd/agent.rs`
//! - `/api/v1/agents/*` (HTTP) — see `mur-core/src/server_agents/`
//! - `mur-agent-runtime` supervisor — see `mur-agent-runtime/src/lock_file.rs`
//!   (the runtime additionally uses `flock`; that check stays local)

use crate::LockFile;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Three-state classification of an agent's runtime state.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatusKind {
    /// Lock present and the recorded pid is alive.
    Running,
    /// Lock present but the pid is not alive (crash/kill — orphan lock).
    Stale,
    /// No lock file.
    Stopped,
}

impl AgentStatusKind {
    /// Stable visual marker for this status — the single source of truth for
    /// the status→emoji mapping used by `mur agent list` and the agent card.
    /// Exhaustive match: adding a variant is a compile error here by design.
    pub fn emoji(&self) -> &'static str {
        match self {
            AgentStatusKind::Running => "🟢",
            AgentStatusKind::Stale => "🟡",
            AgentStatusKind::Stopped => "⚪",
        }
    }
}

/// Result of classifying an agent's lock state.
#[derive(Debug, Clone, Copy)]
pub struct AgentStatus {
    pub kind: AgentStatusKind,
    /// PID from the lock file. `None` when no lock or unparseable lock.
    pub pid: Option<u32>,
}

/// Read and JSON-parse `<home>/running.lock`. Returns:
/// - `Ok(None)` if the file does not exist (agent stopped).
/// - `Ok(Some(_))` if the file exists and parses successfully.
/// - `Err(_)` if the file exists but I/O fails or JSON is malformed.
pub fn read(lock_path: &Path) -> std::io::Result<Option<LockFile>> {
    let bytes = match std::fs::read(lock_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Is the given pid currently a live process the calling user can signal?
///
/// On Unix uses `kill(pid, 0)` — signal 0 is a no-op probe that checks
/// process existence and permission without delivering any signal.
///
/// On Windows uses `OpenProcess` with `PROCESS_QUERY_LIMITED_INFORMATION`,
/// then `GetExitCodeProcess` — an openable handle is NOT proof of life.
///
/// On other platforms returns `true` (optimistically treat any present lock
/// as live, since P0a agents are not supported there).
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill(2) with signal 0 delivers no signal; it only checks
    // process existence and our permission to signal it. Always safe to call.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: OpenProcess/GetExitCodeProcess/CloseHandle are safe to call with
    // any pid; the handle is closed on every path out.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        // A terminated process stays openable until the last handle to it is
        // closed — Windows' equivalent of a zombie. Treating a non-null handle
        // as "alive" therefore reports a just-crashed agent as running, and
        // `clear_runtime_state` then refuses to clear its stale lock. Ask for
        // the exit code instead: only STILL_ACTIVE means running. (Known
        // Windows caveat: a process that exits with code 259 is
        // indistinguishable from a running one. Our runtimes never exit 259,
        // and the failure direction is the conservative one — a lock kept, not
        // a live process's lock deleted.)
        let mut code: u32 = 0;
        let queried = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        // If the query itself fails we cannot tell, so claim alive: keeping a
        // dead process's lock costs the user one manual clear, while deleting
        // a live one's lock lets a second supervisor run against the same agent
        // home (#790).
        queried == 0 || code == STILL_ACTIVE as u32
    }
}

#[cfg(not(any(unix, windows)))]
pub fn pid_alive(_pid: u32) -> bool {
    true
}

/// Classify the agent's running state by inspecting `<home>/running.lock`.
///
/// - No lock → `Stopped`
/// - Lock present, parses, pid alive → `Running`
/// - Lock present but pid not alive (crash / SIGKILL / OOM) → `Stale`
/// - Lock present but unparseable / unreadable → `Stale` with `pid: None`
///   (treat as stale rather than running so dashboards don't paint dead
///   agents green)
pub fn classify(lock_path: &Path) -> AgentStatus {
    match read(lock_path) {
        Ok(None) => AgentStatus {
            kind: AgentStatusKind::Stopped,
            pid: None,
        },
        Err(_) => AgentStatus {
            kind: AgentStatusKind::Stale,
            pid: None,
        },
        Ok(Some(lock)) => {
            let kind = if pid_alive(lock.pid) {
                AgentStatusKind::Running
            } else {
                AgentStatusKind::Stale
            };
            AgentStatus {
                kind,
                pid: Some(lock.pid),
            }
        }
    }
}

/// The runtime's startup exclusion file in an agent's directory. A booting
/// runtime holds its flock for the whole seal, before `running.lock` exists.
pub const SENTINEL_FILE: &str = "running.sentinel";

/// The flock sentinel beside a runtime lock file (`running.lock` →
/// `running.sentinel`).
///
/// A separate file on purpose: `running.lock` is written by temp-file +
/// rename, which swaps its inode. A flock on the JSON file itself would be
/// left on an inode nobody can see; the sentinel is never renamed, so its
/// inode is the one every holder and every probe meets on.
pub fn sentinel_path(lock: &Path) -> PathBuf {
    lock.with_extension("sentinel")
}

/// What [`probe_sentinel`] found on an agent's `running.sentinel`.
#[derive(Debug)]
pub enum SentinelProbe {
    /// The agent directory does not exist, so there is nothing to clear.
    NoAgentDir,
    /// A live process owns it — or the probe could not tell, which counts
    /// the same: keeping a dead runtime's files costs one manual clear,
    /// deleting a live one's state is #790.
    Owned,
    /// The caller now holds its flock and it names no live pid. The handle
    /// keeps the flock until dropped; the dead pid it recorded is erased.
    Free(std::fs::File),
}

/// Take the `running.sentinel` flock if nobody owns it, creating the file
/// when it is missing (the same open the runtime does, so both always meet
/// on one inode).
///
/// The pid the runtime writes into the sentinel is a fallback for a
/// filesystem where the flock probe cannot see a holder. It is erased once
/// the sentinel is judged free, so a later process reusing that pid cannot
/// make a long-dead runtime look alive forever.
///
/// The sentinel itself is never unlinked, by anyone: its existence means
/// nothing, only the flock does, and every unlink is a chance for a holder
/// to end up locking an inode the next opener cannot see. Hold a `Free`
/// handle across any clear of the agent's other files, so a runtime racing
/// the clear fails its own `try_lock_exclusive` and exits as a duplicate —
/// and drop it before launching a runtime, for the same reason.
pub fn probe_sentinel(agent_dir: &Path) -> SentinelProbe {
    use fs2::FileExt;
    use std::io::Read;

    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(agent_dir.join(SENTINEL_FILE))
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SentinelProbe::NoAgentDir,
        Err(_) => return SentinelProbe::Owned,
    };
    if file.try_lock_exclusive().is_err() {
        return SentinelProbe::Owned;
    }
    let mut pid = String::new();
    if file.read_to_string(&mut pid).is_err() {
        return SentinelProbe::Owned;
    }
    if pid.trim().parse::<u32>().is_ok_and(pid_alive) {
        return SentinelProbe::Owned;
    }
    let _ = file.set_len(0);
    SentinelProbe::Free(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::LockTransports;

    #[test]
    fn status_emoji_mapping_is_stable() {
        assert_eq!(AgentStatusKind::Running.emoji(), "🟢");
        assert_eq!(AgentStatusKind::Stale.emoji(), "🟡");
        assert_eq!(AgentStatusKind::Stopped.emoji(), "⚪");
    }

    fn make_lock(pid: u32) -> LockFile {
        LockFile {
            schema: 1,
            uuid: "01JQX4TM8Y9K7VQH6B2N3R5DPE".into(),
            name: "agent_a".into(),
            pid,
            ppid: 1,
            started_at: "2026-04-22T08:00:00Z".into(),
            binary_version: "mur-agent-runtime 0.1.0".into(),
            transports: LockTransports {
                stdio: false,
                unix_socket: Some("/tmp/x.sock".into()),
                tcp: None,
                webhook: None,
            },
            card_digest: "sha256:abc".into(),
            capabilities: vec!["a2a.message.send".into()],
            build_sha: String::new(),
            proto_version: 0,
            sandbox: None,
            startup_override: None,
            override_rejected: None,
        }
    }

    fn write_lock_file(dir: &std::path::Path, pid: u32) -> std::path::PathBuf {
        let path = dir.join("running.lock");
        let lock = make_lock(pid);
        std::fs::write(&path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
        path
    }

    #[test]
    fn classify_returns_stopped_when_no_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("running.lock");
        let status = classify(&lock_path);
        assert_eq!(status.kind, AgentStatusKind::Stopped);
        assert_eq!(status.pid, None);
    }

    #[cfg(unix)]
    #[test]
    fn classify_returns_running_when_pid_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = write_lock_file(tmp.path(), std::process::id());
        let status = classify(&lock_path);
        assert_eq!(status.kind, AgentStatusKind::Running);
        assert_eq!(status.pid, Some(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn classify_returns_stale_when_pid_dead() {
        let tmp = tempfile::tempdir().unwrap();
        let dead_pid: u32 = 999_999;
        let lock_path = write_lock_file(tmp.path(), dead_pid);
        let status = classify(&lock_path);
        assert_eq!(status.kind, AgentStatusKind::Stale);
        assert_eq!(status.pid, Some(dead_pid));
    }

    #[test]
    fn classify_returns_stale_when_lock_malformed() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("running.lock");
        std::fs::write(&lock_path, b"not json").unwrap();
        let status = classify(&lock_path);
        assert_eq!(status.kind, AgentStatusKind::Stale);
        assert_eq!(status.pid, None);
    }

    #[test]
    fn read_returns_none_for_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("running.lock");
        let result = read(&lock_path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_returns_ok_for_valid_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = write_lock_file(tmp.path(), 42);
        let result = read(&lock_path).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().pid, 42);
    }

    #[test]
    fn read_returns_err_for_malformed_json() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("running.lock");
        std::fs::write(&lock_path, b"not json").unwrap();
        let result = read(&lock_path);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_returns_true_for_self() {
        assert!(pid_alive(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_returns_false_for_dead_pid() {
        assert!(!pid_alive(999_999));
    }

    #[test]
    fn sentinel_path_sits_beside_the_lock() {
        let dir = Path::new("/x/agents/a");
        assert_eq!(
            sentinel_path(&dir.join("running.lock")),
            dir.join(SENTINEL_FILE)
        );
    }

    /// Every verdict the probe can reach. Each `Free` handle is dropped
    /// before the next directory is looked at.
    #[test]
    fn probe_sentinel_verdicts() {
        use fs2::FileExt;

        #[derive(Debug, PartialEq)]
        enum V {
            NoDir,
            Owned,
            Free,
        }
        let verdict = |dir: &Path| match probe_sentinel(dir) {
            SentinelProbe::NoAgentDir => V::NoDir,
            SentinelProbe::Owned => V::Owned,
            SentinelProbe::Free(_) => V::Free,
        };

        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing");
        let fresh = tmp.path().join("fresh");
        let dead = tmp.path().join("dead");
        let live = tmp.path().join("live");
        let held = tmp.path().join("held");
        for d in [&fresh, &dead, &live, &held] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(dead.join(SENTINEL_FILE), b"999999").unwrap();
        std::fs::write(live.join(SENTINEL_FILE), std::process::id().to_string()).unwrap();
        std::fs::write(held.join(SENTINEL_FILE), b"").unwrap();
        let flock = std::fs::File::open(held.join(SENTINEL_FILE)).unwrap();
        flock.try_lock_exclusive().unwrap();

        for (dir, want) in [
            (&missing, V::NoDir),
            (&fresh, V::Free),
            (&dead, V::Free),
            (&live, V::Owned),
            (&held, V::Owned),
        ] {
            assert_eq!(verdict(dir), want, "{}", dir.display());
        }
        assert!(!missing.exists(), "a probe must not create the agent dir");
        assert_eq!(
            std::fs::read_to_string(dead.join(SENTINEL_FILE)).unwrap(),
            "",
            "a free sentinel's dead pid is erased"
        );
        drop(flock);
    }
}
