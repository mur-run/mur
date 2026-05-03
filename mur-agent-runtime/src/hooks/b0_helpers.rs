//! Pure helpers for B0SafetyHook rule branches.
//!
//! Each helper is a free function with no IO and no Tauri/runtime
//! state, so unit tests can construct fixtures directly. The helpers
//! are imported by `mur-agent-runtime/src/hooks/b0.rs` from the rule
//! branches that need them.

use std::path::Path;

/// Returns `true` when `candidate` is inside `confine_to` (after
/// canonicalization). A `candidate` that does NOT exist is checked
/// against the parent's canonical path — useful for fs.write where
/// the file may be about to be created.
///
/// Symlinks ARE followed (`canonicalize` resolves them) so this is a
/// real-world confinement check, not a string-prefix match.
pub fn path_confined_to(candidate: &Path, confine_to: &Path) -> bool {
    let confine_canonical = match std::fs::canonicalize(confine_to) {
        Ok(p) => p,
        Err(_) => return false, // confine_to missing — fail closed
    };
    let candidate_canonical = match std::fs::canonicalize(candidate) {
        Ok(p) => p,
        Err(_) => {
            // Not yet created. Check the parent.
            match candidate.parent() {
                Some(parent) => match std::fs::canonicalize(parent) {
                    Ok(p) => p,
                    Err(_) => return false,
                },
                None => return false,
            }
        }
    };
    candidate_canonical.starts_with(&confine_canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn confined_path_is_inside() {
        let dir = TempDir::new().unwrap();
        let inner = dir.path().join("inside.txt");
        std::fs::write(&inner, "x").unwrap();
        assert!(path_confined_to(&inner, dir.path()));
    }

    #[test]
    fn outside_path_rejected() {
        let dir = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let foreign = other.path().join("file.txt");
        std::fs::write(&foreign, "x").unwrap();
        assert!(!path_confined_to(&foreign, dir.path()));
    }

    #[test]
    fn nonexistent_file_uses_parent_for_check() {
        let dir = TempDir::new().unwrap();
        let new_file = dir.path().join("doesnt-exist-yet.txt");
        // Parent (dir) exists and IS the confine root.
        assert!(path_confined_to(&new_file, dir.path()));
    }

    #[test]
    fn nonexistent_parent_fails_closed() {
        let dir = TempDir::new().unwrap();
        let two_deep = dir.path().join("ghost-dir/file.txt");
        assert!(!path_confined_to(&two_deep, dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_outside_rejected() {
        let confine = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let target = other.path().join("real.txt");
        std::fs::write(&target, "x").unwrap();
        let link = confine.path().join("escape.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // Symlink resolves outside confine_to → reject.
        assert!(!path_confined_to(&link, confine.path()));
    }
}
