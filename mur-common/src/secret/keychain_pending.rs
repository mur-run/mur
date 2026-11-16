//! Markers for user secrets whose keychain read timed out before the seal (#1980).
//!
//! A keychain item this binary has no "Always Allow" ACL for makes macOS raise
//! a modal prompt. The runtime gives up after a short timeout, but the prompt
//! stays on screen. Under a service-manager crash loop every respawn read again, and
//! each one left another dialog: dozens in a few minutes.
//!
//! The runtime writes one marker per timed-out secret and skips the probe on
//! later starts. A crash loop then costs one prompt per secret, not one per
//! process. Actions the user takes clear the markers: `mur agent secret
//! set|delete`, murmur `/secret`, `mur agent start` and `mur agent restart`
//! (service-manager respawns go through none of these). Clearing makes the next start
//! probe again. If the prompt has since been answered with Always Allow, the
//! read succeeds without a dialog.
//!
//! Layout: `<agent_home>/keychain-pending/<SECRET_NAME>`, holding the RFC 3339
//! time it was written. Every operation is best-effort. A marker that cannot be
//! written costs one extra prompt, and a marker that cannot be read leaves the
//! old behaviour in place. Neither is worth failing a start over.

use std::path::{Path, PathBuf};

/// Directory under the agent home that holds one marker file per secret name.
pub const PENDING_DIR: &str = "keychain-pending";

fn dir(agent_home: &Path) -> PathBuf {
    agent_home.join(PENDING_DIR)
}

/// Secret names are `[A-Z_][A-Z0-9_]*` (enforced on every write path). Anything
/// else is refused here so a hand-edited profile cannot turn a name into a path.
fn safe_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Records that `name`'s keychain read timed out.
pub fn mark(agent_home: &Path, name: &str) -> std::io::Result<()> {
    if !safe_name(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a secret name: {name:?}"),
        ));
    }
    let d = dir(agent_home);
    std::fs::create_dir_all(&d)?;
    std::fs::write(d.join(name), chrono::Utc::now().to_rfc3339())
}

/// Whether a start should skip the keychain probe for `name`.
pub fn is_pending(agent_home: &Path, name: &str) -> bool {
    safe_name(name) && dir(agent_home).join(name).is_file()
}

/// Removes the marker for one secret. Returns true if a marker was removed.
pub fn clear(agent_home: &Path, name: &str) -> bool {
    safe_name(name) && std::fs::remove_file(dir(agent_home).join(name)).is_ok()
}

/// Removes every marker for an agent. Returns how many were removed.
pub fn clear_all(agent_home: &Path) -> usize {
    let names = list(agent_home);
    names.iter().filter(|n| clear(agent_home, n)).count()
}

/// The secret names waiting for keychain approval, sorted.
pub fn list(agent_home: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir(agent_home)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| safe_name(n))
        .collect();
    names.sort();
    names
}

/// File under the agent home saying a person just ran `mur agent start` or
/// `restart` (#2369). Kept outside [`PENDING_DIR`] so [`list`] never reports
/// it as a secret.
pub const ATTENDED_FILE: &str = "keychain-attended";

/// How long an [`ATTENDED_FILE`] counts. It covers the gap between the
/// command and the runtime reaching its seal, including a slow service kick.
pub const ATTENDED_FRESH: std::time::Duration = std::time::Duration::from_secs(300);

/// Records that a person is at the machine for this start, so the runtime may
/// wait longer for a Keychain prompt to be answered. Best-effort.
pub fn mark_attended(agent_home: &Path) {
    let _ = std::fs::write(
        agent_home.join(ATTENDED_FILE),
        chrono::Utc::now().to_rfc3339(),
    );
}

/// Whether this start was asked for by a person within [`ATTENDED_FRESH`].
/// Consumes the marker: a later crash-loop respawn is unattended again.
pub fn take_attended(agent_home: &Path) -> bool {
    let path = agent_home.join(ATTENDED_FILE);
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age <= ATTENDED_FRESH);
    let _ = std::fs::remove_file(&path);
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_then_skip_until_cleared() {
        let home = tempfile::tempdir().unwrap();
        assert!(!is_pending(home.path(), "GOOGLE_MAPS_KEY"));
        mark(home.path(), "GOOGLE_MAPS_KEY").unwrap();
        assert!(is_pending(home.path(), "GOOGLE_MAPS_KEY"));
        // A second start must still see it: one prompt per secret, not per start.
        assert!(is_pending(home.path(), "GOOGLE_MAPS_KEY"));
        assert!(!is_pending(home.path(), "OTHER_KEY"));
        assert!(clear(home.path(), "GOOGLE_MAPS_KEY"));
        assert!(!is_pending(home.path(), "GOOGLE_MAPS_KEY"));
        assert!(!clear(home.path(), "GOOGLE_MAPS_KEY"));
    }

    #[test]
    fn list_and_clear_all() {
        let home = tempfile::tempdir().unwrap();
        assert!(list(home.path()).is_empty());
        mark(home.path(), "B_KEY").unwrap();
        mark(home.path(), "A_KEY").unwrap();
        assert_eq!(list(home.path()), vec!["A_KEY", "B_KEY"]);
        assert_eq!(clear_all(home.path()), 2);
        assert!(list(home.path()).is_empty());
    }

    #[test]
    fn attended_is_taken_once_and_never_listed() {
        let home = tempfile::tempdir().unwrap();
        assert!(!take_attended(home.path()));
        mark_attended(home.path());
        assert!(list(home.path()).is_empty());
        assert!(take_attended(home.path()));
        // A respawn after the first start is unattended again.
        assert!(!take_attended(home.path()));
    }

    #[test]
    fn path_like_names_are_refused() {
        let home = tempfile::tempdir().unwrap();
        assert!(mark(home.path(), "../escape").is_err());
        assert!(mark(home.path(), "").is_err());
        assert!(!is_pending(home.path(), "../escape"));
        assert!(!home.path().parent().unwrap().join("escape").exists());
    }
}
