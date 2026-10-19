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
    fn path_like_names_are_refused() {
        let home = tempfile::tempdir().unwrap();
        assert!(mark(home.path(), "../escape").is_err());
        assert!(mark(home.path(), "").is_err());
        assert!(!is_pending(home.path(), "../escape"));
        assert!(!home.path().parent().unwrap().join("escape").exists());
    }
}
