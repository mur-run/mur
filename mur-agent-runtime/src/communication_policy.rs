//! sends_to (intent filter) and accepts_from (authoritative security boundary).

use std::path::Path;

pub fn sends_to_allows(list: &[String], peer: &str) -> bool {
    list.iter().any(|p| glob_match(p, peer))
}

pub fn accepts_from_allows(list: &[String], caller: &str) -> bool {
    list.iter().any(|p| glob_match(p, caller))
}

fn glob_match(pattern: &str, s: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match glob::Pattern::new(pattern) {
        Ok(p) => p.matches(s),
        Err(_) => pattern == s,
    }
}

/// Given an agents directory, return the name whose running.lock's pid matches.
/// Returns None if not found (common for CLI callers — treat as trusted user).
pub fn resolve_caller_name(agents_dir: &Path, caller_pid: u32) -> Option<String> {
    let entries = std::fs::read_dir(agents_dir).ok()?;
    for entry in entries.flatten() {
        let lock_path = entry.path().join("running.lock");
        if !lock_path.exists() {
            continue;
        }
        let bytes = std::fs::read(&lock_path).ok()?;
        let lock: mur_common::LockFile = serde_json::from_slice(&bytes).ok()?;
        if lock.pid == caller_pid {
            return Some(lock.name);
        }
    }
    None
}
