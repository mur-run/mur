//! Shared staleness helpers — used by `mur agent restart` (Task 6)
//! and `mur agent doctor` (Task 7).
//!
//! A running agent is "stale" when the binary on disk has a different
//! `--build-id` than the sha recorded in its `running.lock`.

use std::process::Command;

use mur_common::LockFile;

use super::resolve_runtime_target;

/// Run `<runtime> --build-id` and return the trimmed stdout, or `"unknown"`
/// if the binary is missing / the sub-command fails.
pub fn on_disk_sha() -> String {
    let runtime = resolve_runtime_target();
    let out = Command::new(&runtime).arg("--build-id").output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { "unknown".to_string() } else { s }
        }
        _ => "unknown".to_string(),
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
