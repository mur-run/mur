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

/// Build-id of the runtime sitting next to the current `mur` binary.
///
/// Only correct as a staleness baseline for an agent whose own symlink points
/// here — prefer [`on_disk_sha_for`], which cannot get that wrong.
pub fn on_disk_sha() -> String {
    build_id(&resolve_runtime_target())
}

/// Build-id of the binary that will *actually* be exec'd for `agent`.
///
/// A restart does not run the runtime next to `mur`; it runs the agent's own
/// `~/.local/bin/mur_agent_<name>` — that path is literally `ProgramArguments[0]`
/// in the service descriptor, and those symlinks do not all point at the same
/// runtime (a dev checkout, an older keg, and `mur update`'s copy can coexist).
/// Comparing every agent against one global runtime therefore answers a question
/// nobody asked: `--stale` can restart an agent that comes back on the same old
/// binary, and stay silent about one that is genuinely behind.
///
/// Falls back to the global runtime when the agent has no symlink yet.
pub fn on_disk_sha_for(agent: &str) -> String {
    match resolve_bin_dir().map(|d| d.join(format!("mur_agent_{agent}"))) {
        Ok(link) if link.symlink_metadata().is_ok() => build_id(&link),
        _ => on_disk_sha(),
    }
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
}
