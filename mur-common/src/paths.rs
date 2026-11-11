//! Directory names under `<mur_home>` that a fleet / deep-research run writes.
//!
//! One list, two readers that must never disagree: `mur-core` creates and
//! writes these while a run executes, and `mur-agent-runtime` carves exactly
//! these into the kernel sandbox for an agent-triggered run (`fleet_run`).
//! They lived as two separate literals and drifted — `runs/` was writable by
//! the CLI and denied to the sandbox, so every agent-triggered run failed to
//! register itself and `mur fleet status` reported nothing for a run that was
//! live (fleet develop-rust, 2026-09-09). The list lives here because the
//! runtime must not depend on `mur-core`.

use std::path::{Path, PathBuf};

/// Run-status records of runs that belong to no fleet (`parallel_jobs`,
/// workflows): `<mur_home>/runs/<run_id>/run.json`.
///
/// A FLEET run's directory is never here — it is
/// [`fleet_run_dir`], inside the per-fleet carve-in. `runs/` is writable by
/// writable by no sandboxed agent since #2320, but it was before, so
/// anything a fleet resume trusts still must not be read from it (#2316).
/// Runs an agent dispatches go to [`agent_runs_dir`] instead.
pub const RUNS: &str = "runs";

/// Fleet definitions: `<mur_home>/fleets/<name>/fleet.yaml`, plus the
/// operator's `.stopped` kill-switch and the daemon's `.last_run` stamp.
///
/// Configuration, not run state — deliberately absent from
/// [`RUN_STATE_DIRS`]. A run that could write here could rewrite its own
/// fleet's members, limits and HITL pre-approvals, or clear its own
/// kill-switch.
pub const FLEETS: &str = "fleets";

/// What a fleet's runs write, per fleet: `<mur_home>/fleet-state/<name>/`
/// (job queue, progress record, event log, parallel-track state).
///
/// Its own tree, not `runs/`: that store is keyed by run_id and lists every
/// directory in it as a run, so a fleet name dropped in there reads as a
/// phantom run. Not `fleets/`: that is configuration (see [`FLEETS`]).
pub const FLEET_STATE: &str = "fleet-state";

/// The fleet's single progress record, inside [`fleet_state_dir`]. Kept after
/// a run as the last-run record; overwritten by the next run. Read by both
/// the CLI and the runtime's `fleet_run` live-run guard, so named once here.
pub const FLEET_PROGRESS_FILE: &str = ".run_progress.json";

/// Exclusive advisory lock held for the life of a fleet run, inside
/// [`fleet_state_dir`]. Liveness is whether the lock can be taken, never a
/// stored pid. The holder's self-description lives in [`FLEET_RUN_OWNER_FILE`]
/// because Windows locks are mandatory and a held file cannot be read.
pub const FLEET_RUN_LOCK_FILE: &str = "run.lock";

/// Display-only owner record (pid, start time, host) beside the run lock.
pub const FLEET_RUN_OWNER_FILE: &str = "run.owner";

/// Every directory an in-sandbox fleet run must be able to write.
///
/// Add here FIRST, then use it. A directory the runner writes but this list
/// omits fails only in the sandboxed path — the one nobody runs by hand.
///
/// [`FLEET_STATE`] is listed for the CLI side and for tests; the sandbox
/// carves in only the `fleet_run.fleets` subdirectories of it (see
/// `mur-agent-runtime` `sandbox/policy/build.rs`), so an agent cannot queue work
/// for a fleet the operator never let it run.
///
/// `commander/` is deliberately absent: it holds the commander's private key
/// and `.env`, and its public keys are trust anchors. A run writes nothing
/// there; the sandbox grants read on the two public key files instead.
///
/// [`RUNS`] is deliberately absent (#2320): it holds the operator's own
/// non-fleet runs, and a whole-tree grant let every allowlisted agent rewrite
/// every other agent's records. An agent's own runs go to
/// [`agent_runs_dir`], inside its home.
pub const RUN_STATE_DIRS: [&str; 3] = [FLEET_STATE, "conversations", "artifacts"];

/// Agent homes: `<mur_home>/agents/<name>/`.
pub const AGENTS: &str = "agents";

/// Env var naming the agent a child process runs for, set by the agent
/// runtime on its bash and MCP children. A run recorded by that child goes to
/// [`agent_runs_dir`] of this agent.
///
/// Not a credential: the path it names is the agent's own home, which the
/// kernel sandbox lets no other agent write. A forged value only makes the
/// record fail to write.
pub const RUN_OWNER_ENV: &str = "MUR_RUN_OWNER";

/// `<mur_home>/agents/<agent>/runs/` — the non-fleet runs (`parallel_jobs`,
/// workflows) `agent` dispatched (#2320). Inside the agent's home, so only
/// that agent (and the operator) can write them.
pub fn agent_runs_dir(mur_home: &Path, agent: &str) -> PathBuf {
    mur_home.join(AGENTS).join(agent).join(RUNS)
}

/// The run owner named by [`RUN_OWNER_ENV`], if set to a valid agent name.
/// An invalid name is ignored rather than joined: it becomes a path segment.
pub fn run_owner_from_env() -> Option<String> {
    std::env::var(RUN_OWNER_ENV)
        .ok()
        .filter(|n| crate::agent_name::validate_agent_name(n).is_ok())
}

/// `<mur_home>/fleet-state/<name>/` — every file a run of `name` writes.
pub fn fleet_state_dir(mur_home: &Path, name: &str) -> PathBuf {
    mur_home.join(FLEET_STATE).join(name)
}

/// `<mur_home>/fleet-state/<name>/runs/` — the run directories of `name`'s
/// runs (`run.json`, `sidecar.json`, `checkpoint.json`, logs, the daemon's
/// `dispatch.json`). Inside [`fleet_state_dir`] so only an agent allowed to
/// run `name` can write them (#2316).
pub fn fleet_runs_dir(mur_home: &Path, name: &str) -> PathBuf {
    fleet_state_dir(mur_home, name).join(RUNS)
}

/// `<mur_home>/fleet-state/<name>/runs/<run_id>/`.
pub fn fleet_run_dir(mur_home: &Path, name: &str, run_id: &str) -> PathBuf {
    fleet_runs_dir(mur_home, name).join(run_id)
}

/// Is `s` safe to join as ONE path segment under a runs directory? Letters,
/// digits, `-` and `_` only — the same alphabet `mur fleet run --run-id`
/// accepts — so no `/`, `..`, or empty segment can steer a lookup elsewhere.
pub fn is_single_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= RUN_ID_MAX_LEN
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Longest run id accepted as a path segment.
pub const RUN_ID_MAX_LEN: usize = 96;

/// Read `<run_dir>/<file>` only when both are what they look like: `run_dir`
/// is a real directory and `file` a regular file with one link. Anything a
/// resume or pause TRUSTS is read through this (#2316).
///
/// A fleet run directory sits in the fleet's sandbox carve-in, but the path
/// alone does not make its bytes the fleet's: a symlink, or a hard link to a
/// file elsewhere, reads through the trusted path into a tree every agent can
/// write (`runs/`, before the one-time migration moved it). Both are refused
/// as `InvalidData`; a missing file is still `NotFound`, so callers keep
/// their "no record" answer. Windows has no stable link count, so there only
/// symlinks are refused.
pub fn read_unlinked(run_dir: &Path, file: &str) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let refuse = |what: &str, p: &Path| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is {what}; refusing to trust it", p.display()),
        )
    };
    if std::fs::symlink_metadata(run_dir)?.file_type().is_symlink() {
        return Err(refuse("a symlink", run_dir));
    }
    let path = run_dir.join(file);
    let mut f = open_no_follow(&path).map_err(|e| {
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            refuse("a symlink", &path)
        } else {
            e
        }
    })?;
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(refuse("not a regular file", &path));
    }
    if link_count(&meta) > 1 {
        return Err(refuse("a hard link to another file", &path));
    }
    let mut out = Vec::new();
    f.read_to_end(&mut out)?;
    Ok(out)
}

/// Open `path` read-only, refusing a symlink at the final component.
#[cfg(unix)]
pub fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// Open `path` read-only, refusing a symlink at the final component.
#[cfg(not(unix))]
pub fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    // No O_NOFOLLOW: check, then open. The window is the caller's own tree.
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "symlink",
        ));
    }
    std::fs::File::open(path)
}

#[cfg(unix)]
fn link_count(meta: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::nlink(meta)
}

#[cfg(not(unix))]
fn link_count(_meta: &std::fs::Metadata) -> u64 {
    1
}

/// Does the tree at `dir` hold anything [`read_unlinked`] would refuse — a
/// symlink anywhere, or a regular file with more than one link? `dir` itself
/// is checked without following it. An unreadable tree answers `true`: the
/// caller is deciding whether to trust it.
pub fn holds_links(dir: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return true;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    if meta.is_file() {
        return link_count(&meta) > 1;
    }
    if !meta.is_dir() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    entries
        .map(|e| e.map(|e| holds_links(&e.path())))
        .any(|r| r.unwrap_or(true))
}

/// Directory holding the daemon's agent-readable heartbeat (#2116).
///
/// Not `murmurd.lock`: a sealed agent must know the daemon is up before it
/// queues a request only the daemon serves, and on Linux it reads only what
/// Landlock grants. A file grant is pinned to the inode present at seal time
/// and is skipped outright when the file does not exist yet (the daemon has
/// not started), so the grant is on this DIRECTORY, which the sandbox builder
/// creates before sealing; the daemon replaces the file inside it with
/// tmp + rename. Only a daemon that sweeps the fleet-dispatch inbox writes
/// it, so an older daemon reads as "down" — correctly, since it would never
/// serve the request.
pub const DAEMON_HEARTBEAT_DIR: &str = "daemon-heartbeat";

/// The heartbeat file inside [`DAEMON_HEARTBEAT_DIR`]: JSON carrying an
/// RFC 3339 `heartbeat_at`.
pub const DAEMON_HEARTBEAT_FILE: &str = "heartbeat.json";

/// `<mur_home>/daemon-heartbeat/heartbeat.json`.
pub fn daemon_heartbeat_path(mur_home: &Path) -> PathBuf {
    mur_home
        .join(DAEMON_HEARTBEAT_DIR)
        .join(DAEMON_HEARTBEAT_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_unlinked_reads_a_plain_file_and_reports_a_missing_one() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("f"), b"ok").unwrap();
        assert_eq!(read_unlinked(tmp.path(), "f").unwrap(), b"ok");
        let missing = read_unlinked(tmp.path(), "nope").unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
        assert!(!holds_links(tmp.path()));
    }

    #[cfg(unix)]
    #[test]
    fn read_unlinked_refuses_a_symlinked_file_or_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("f"), b"forged").unwrap();

        let dir = tmp.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        std::os::unix::fs::symlink(outside.join("f"), dir.join("f")).unwrap();
        let e = read_unlinked(&dir, "f").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        assert!(holds_links(&dir));

        let linked_dir = tmp.path().join("run2");
        std::os::unix::fs::symlink(&outside, &linked_dir).unwrap();
        let e = read_unlinked(&linked_dir, "f").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        assert!(holds_links(&linked_dir));
    }

    #[cfg(unix)]
    #[test]
    fn read_unlinked_refuses_a_hard_link_to_a_file_elsewhere() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("elsewhere"), b"forged").unwrap();
        let dir = tmp.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::hard_link(tmp.path().join("elsewhere"), dir.join("f")).unwrap();
        let e = read_unlinked(&dir, "f").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        assert!(holds_links(&dir));
    }

    #[test]
    fn runs_is_not_run_state() {
        // #2320: fleet runs live under `fleet-state/` and an agent's own runs
        // under its home, so nothing a sandboxed run writes needs `runs/` —
        // and granting it whole let one agent rewrite another's records.
        assert!(!RUN_STATE_DIRS.contains(&RUNS));
    }

    #[test]
    fn agent_runs_live_in_the_agent_home() {
        let d = agent_runs_dir(Path::new("/m"), "bob");
        assert_eq!(d, Path::new("/m").join(AGENTS).join("bob").join(RUNS));
    }

    #[test]
    fn fleet_definitions_are_not_run_state() {
        // The whole point of FLEET_STATE: a run writes there, never into
        // the tree that holds the fleet's own definition and kill-switch.
        assert!(!RUN_STATE_DIRS.contains(&FLEETS));
        assert!(RUN_STATE_DIRS.contains(&FLEET_STATE));
        assert_ne!(FLEET_STATE, RUNS);
    }

    #[test]
    fn commander_is_not_run_state() {
        // A grant on `commander/` reaches the commander's private key, its
        // `.env` and the pinned public keys directives are verified against.
        assert!(!RUN_STATE_DIRS.contains(&crate::commander::COMMANDER_DIR));
    }

    #[test]
    fn fleet_definitions_are_not_an_authoring_grant() {
        // Same boundary, the other writer: the seeded concierge gets write on
        // every AUTHORING_DIRS entry, so `fleets/` there hands it the fleet's
        // members, limits, HITL pre-approvals and `.stopped` kill-switch.
        assert!(!crate::agent::AUTHORING_DIRS.contains(&FLEETS));
        assert!(!crate::agent::AUTHORING_DIRS.contains(&FLEET_STATE));
    }
}
