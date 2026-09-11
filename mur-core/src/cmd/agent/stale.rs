//! Shared staleness helpers — used by `mur agent restart` (Task 6)
//! and `mur agent doctor` (Task 7).
//!
//! A running agent is "stale" when the binary on disk has a different
//! `--build-id` than the sha recorded in its `running.lock`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use mur_common::LockFile;

use super::{resolve_bin_dir, resolve_runtime_target};

/// Run `<bin> --build-id` and return the trimmed stdout, or `"unknown"` if the
/// binary is missing / the sub-command fails.
///
/// Memoized by resolved path: `--stale` asks this once per agent, and on a
/// normal install every agent's symlink resolves to the same runtime — so the
/// cache turns N subprocess spawns into one per distinct binary.
fn build_id(bin: &Path) -> String {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    if let Some(hit) = cache.lock().expect("build-id cache").get(&key) {
        return hit.clone();
    }
    let sha = match Command::new(bin).arg("--build-id").output() {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                "unknown".to_string()
            } else {
                s
            }
        }
        _ => "unknown".to_string(),
    };
    cache
        .lock()
        .expect("build-id cache")
        .insert(key, sha.clone());
    sha
}

/// The runtime binary that will *actually* be exec'd for `agent`.
///
/// A restart does not run the runtime next to `mur`; it runs the agent's own
/// `~/.local/bin/mur_agent_<name>` — that path is literally `ProgramArguments[0]`
/// in the service descriptor, and those symlinks do not all point at the same
/// runtime (a dev checkout, an older keg, and `mur update`'s copy can coexist).
///
/// This is the single source of truth for "which binary belongs to this agent".
/// Both the staleness verdict and the direct respawn read it, because when they
/// each resolved it their own way they disagreed: the verdict measured the
/// agent's symlink while the respawn exec'd the runtime beside `mur`, so a
/// restart could relaunch the very binary it had just called stale and still
/// print a tick.
///
/// Falls back to the global runtime when the agent has no symlink yet.
pub fn runtime_path_for(agent: &str) -> PathBuf {
    match resolve_bin_dir().map(|d| d.join(format!("mur_agent_{agent}"))) {
        Ok(link) if link.symlink_metadata().is_ok() => link,
        _ => resolve_runtime_target(),
    }
}

/// Build-id of the binary that will *actually* be exec'd for `agent`.
///
/// Comparing every agent against one global runtime answers a question nobody
/// asked: `--stale` can restart an agent that comes back on the same old
/// binary, and stay silent about one that is genuinely behind.
pub fn on_disk_sha_for(agent: &str) -> String {
    build_id(&runtime_path_for(agent))
}

/// Return `true` when the agent whose lock is `lock` is running a stale binary.
///
/// Rules:
/// - Different non-empty, non-unknown shas → stale.
/// - Both `"unknown"` → NOT stale (we can't tell, assume equal).
/// - Empty `build_sha` in lock (old pre-feature lock) AND on-disk sha is known → stale.
/// - Same sha → NOT stale.
pub fn is_stale(lock: &LockFile, on_disk: &str) -> bool {
    let running = lock.build_sha.as_str();
    match (running, on_disk) {
        // Both unknown → treat as equal (no information)
        ("unknown", "unknown") => false,
        // Empty running sha (old lock) + known on-disk → stale
        ("", od) if od != "unknown" => true,
        // Empty running sha + on-disk also unknown → can't tell, not stale
        ("", _) => false,
        // Same sha → not stale
        (r, od) if r == od => false,
        // Different shas → stale
        _ => true,
    }
}

/// Where an agent's launcher symlink resolves, when that is not the canonical
/// runtime in the same bin dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkDrift {
    /// What `mur_agent_<name>` actually resolves to today.
    pub points_at: PathBuf,
    /// `<bin_dir>/mur-agent-runtime` — the copy every installer refreshes.
    pub canonical: PathBuf,
}

/// Report an agent whose launcher points somewhere other than the canonical
/// runtime beside it, given an explicit `bin_dir` (the testable core).
///
/// `None` when there is nothing to say: no launcher yet, no canonical copy to
/// point at, or the launcher already resolves to it. Both sides are resolved
/// through symlinks before comparing, because the Homebrew path is itself a
/// symlink into the versioned keg — comparing the literal link targets would
/// call two names for the same file a drift.
///
/// Why this matters beyond tidiness: a launcher pinned into the keg is a path
/// no installer refreshes, so the agent silently never upgrades, and on macOS
/// a Full Disk Access grant on the sensible path never applies because TCC
/// keys on the binary actually executed (issue #1247).
pub fn link_drift_in(bin_dir: &Path, agent: &str) -> Option<LinkDrift> {
    let link = bin_dir.join(format!("mur_agent_{agent}"));
    link.symlink_metadata().ok()?;
    let canonical = bin_dir.join(if cfg!(windows) {
        "mur-agent-runtime.exe"
    } else {
        "mur-agent-runtime"
    });
    let canonical_real = std::fs::canonicalize(&canonical).ok()?;
    let points_at = std::fs::canonicalize(&link).ok()?;
    (points_at != canonical_real).then_some(LinkDrift {
        points_at,
        canonical,
    })
}

/// Point `agent`'s launcher back at the canonical runtime in `bin_dir`.
///
/// Replaces the link rather than editing it in place, and returns the path it
/// now names. The caller decides when this runs: re-pointing changes which
/// binary the next start executes, which is a trust boundary, so it never
/// happens without the user asking.
pub fn repoint_in(bin_dir: &Path, agent: &str) -> std::io::Result<PathBuf> {
    let link = bin_dir.join(format!("mur_agent_{agent}"));
    let canonical = bin_dir.join(if cfg!(windows) {
        "mur-agent-runtime.exe"
    } else {
        "mur-agent-runtime"
    });
    if link.symlink_metadata().is_ok() {
        std::fs::remove_file(&link)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&canonical, &link)?;
    #[cfg(windows)]
    std::fs::copy(&canonical, &link)?;
    Ok(canonical)
}

/// [`link_drift_in`] against the installed bin dir.
pub fn link_drift(agent: &str) -> Option<LinkDrift> {
    link_drift_in(&resolve_bin_dir().ok()?, agent)
}

/// [`repoint_in`] against the installed bin dir.
pub fn repoint(agent: &str) -> anyhow::Result<PathBuf> {
    let dir = resolve_bin_dir()?;
    Ok(repoint_in(&dir, agent)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::LockFile;

    fn make_lock(build_sha: &str) -> LockFile {
        LockFile {
            schema: 1,
            uuid: "test-uuid".to_string(),
            name: "test-agent".to_string(),
            pid: 12345,
            ppid: 1,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            binary_version: "1.0.0".to_string(),
            transports: mur_common::agent::LockTransports {
                stdio: true,
                unix_socket: None,
                tcp: None,
                webhook: None,
            },
            card_digest: "abc".to_string(),
            capabilities: vec![],
            build_sha: build_sha.to_string(),
            proto_version: 1,
            sandbox: None,
        }
    }

    #[test]
    fn is_stale_different_shas_is_true() {
        let lock = make_lock("abc123def456");
        assert!(is_stale(&lock, "999999999999"));
    }

    #[test]
    fn is_stale_same_sha_is_false() {
        let lock = make_lock("abc123def456");
        assert!(!is_stale(&lock, "abc123def456"));
    }

    #[test]
    fn is_stale_two_unknowns_is_false() {
        let lock = make_lock("unknown");
        assert!(!is_stale(&lock, "unknown"));
    }

    #[test]
    fn is_stale_empty_lock_sha_with_known_on_disk_is_true() {
        let lock = make_lock("");
        assert!(is_stale(&lock, "abc123def456"));
    }

    /// #1247: a launcher pinned into the keg is a path no installer refreshes,
    /// so the agent never upgrades and a TCC grant on the canonical path never
    /// applies. Report it, and repair only when asked.
    /// Unix-only: the fixture needs real symlinks, which is also the only shape
    /// this bug takes — on Windows the launcher is a copy, not a link.
    #[cfg(unix)]
    #[test]
    fn link_drift_is_reported_only_when_there_is_somewhere_better_to_point() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        let keg = t.path().join("keg");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&keg).unwrap();
        let keg_rt = keg.join("mur-agent-runtime");
        std::fs::write(&keg_rt, b"old").unwrap();

        // A launcher into the keg, with no canonical copy yet: nothing better
        // to point at, so nothing to report.
        let link = bin.join("mur_agent_qa");
        std::os::unix::fs::symlink(&keg_rt, &link).unwrap();
        assert_eq!(link_drift_in(&bin, "qa"), None, "no canonical copy yet");

        // Canonical copy lands (an install or `mur update`): now it is drift.
        let canonical = bin.join("mur-agent-runtime");
        std::fs::write(&canonical, b"new").unwrap();
        let d = link_drift_in(&bin, "qa").expect("drift reported");
        assert_eq!(d.points_at, std::fs::canonicalize(&keg_rt).unwrap());
        assert_eq!(d.canonical, canonical);

        // Repair, then silence.
        let now = repoint_in(&bin, "qa").unwrap();
        assert_eq!(now, canonical);
        assert_eq!(link_drift_in(&bin, "qa"), None, "repaired");

        // An agent with no launcher at all is not drift.
        assert_eq!(link_drift_in(&bin, "nobody"), None);
    }

    /// The Homebrew path is itself a symlink into the versioned keg, so two
    /// names for one file must not read as drift.
    /// Unix-only for the same reason: the fixture builds a symlink chain.
    #[cfg(unix)]
    #[test]
    fn two_names_for_the_same_file_are_not_drift() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let canonical = bin.join("mur-agent-runtime");
        std::fs::write(&canonical, b"rt").unwrap();
        let alias = t.path().join("alias-runtime");
        std::os::unix::fs::symlink(&canonical, &alias).unwrap();
        std::os::unix::fs::symlink(&alias, bin.join("mur_agent_qa")).unwrap();
        assert_eq!(link_drift_in(&bin, "qa"), None, "same file, different name");
    }
}
