//! `mur agent doctor` check for the human owner's key material (at-mention
//! phase 2, A0).
//!
//! `<mur_home>/identity/` is the public half and must stay 0700; any private
//! key file (`*.key*`, which includes `identity.key.prev`) there or under
//! `<mur_home>/keys/.human/` must be 0600. A key under `identity/` at all is
//! an error by construction — that directory is readable by every agent — and
//! a loose mode on a key elsewhere lets another local user read it.

use std::path::{Path, PathBuf};

use mur_common::identity::human::{human_identity_dir, human_key_dir};

/// Mode the public directory must have.
const IDENTITY_DIR_MODE: u32 = 0o700;
/// Mode every private key file must have.
const KEY_FILE_MODE: u32 = 0o600;
/// Substring that marks a private key file name (`identity.key`, `.key.prev`).
const KEY_MARKER: &str = ".key";

/// One problem, already phrased for the doctor output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFinding {
    pub path: PathBuf,
    pub problem: String,
}

/// Every problem with the human identity's on-disk modes. Empty when nothing
/// is enrolled: an absent directory is not an error.
pub fn findings(mur_home: &Path) -> Vec<KeyFinding> {
    let mut out = Vec::new();
    let public = human_identity_dir(mur_home);
    if let Some(mode) = mode_of(&public)
        && mode != IDENTITY_DIR_MODE
    {
        out.push(KeyFinding {
            path: public.clone(),
            problem: format!("mode {mode:o}, want {IDENTITY_DIR_MODE:o}"),
        });
    }
    for key in key_files(&public) {
        out.push(KeyFinding {
            path: key,
            problem: "private key material in the agent-readable identity/ directory".into(),
        });
    }
    for key in key_files(&human_key_dir(mur_home)) {
        if let Some(mode) = mode_of(&key)
            && mode != KEY_FILE_MODE
        {
            out.push(KeyFinding {
                path: key,
                problem: format!("mode {mode:o}, want {KEY_FILE_MODE:o}"),
            });
        }
    }
    out
}

/// Doctor lines for [`findings`].
pub fn lines(mur_home: &Path) -> Vec<String> {
    findings(mur_home)
        .into_iter()
        .map(|f| format!("ERROR human identity: {}: {}", f.path.display(), f.problem))
        .collect()
}

fn key_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(KEY_MARKER))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

#[cfg(unix)]
fn mode_of(p: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(p)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

/// No POSIX modes to check.
#[cfg(not(unix))]
fn mode_of(_: &Path) -> Option<u32> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn chmod(p: &Path, mode: u32) {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn touch(p: &Path, mode: u32) {
        std::fs::write(p, b"k").unwrap();
        chmod(p, mode);
    }

    #[test]
    fn nothing_enrolled_is_clean() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(findings(tmp.path()).is_empty());
    }

    #[test]
    fn a_correct_layout_is_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let (public, private) = (human_identity_dir(tmp.path()), human_key_dir(tmp.path()));
        std::fs::create_dir_all(&public).unwrap();
        std::fs::create_dir_all(&private).unwrap();
        chmod(&public, IDENTITY_DIR_MODE);
        touch(&public.join("identity.pub"), 0o644);
        touch(&private.join("identity.key"), KEY_FILE_MODE);
        assert_eq!(findings(tmp.path()), vec![]);
    }

    #[test]
    fn doctor_flags_key_material_under_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let (public, private) = (human_identity_dir(tmp.path()), human_key_dir(tmp.path()));
        std::fs::create_dir_all(&public).unwrap();
        std::fs::create_dir_all(&private).unwrap();
        chmod(&public, 0o755);
        // Even at 0600 a key in the public directory is wrong.
        touch(&public.join("identity.key"), KEY_FILE_MODE);
        touch(&private.join("identity.key"), 0o644);
        touch(&private.join("identity.key.prev"), 0o640);
        let got: Vec<PathBuf> = findings(tmp.path()).into_iter().map(|f| f.path).collect();
        assert_eq!(
            got,
            vec![
                public.clone(),
                public.join("identity.key"),
                private.join("identity.key"),
                private.join("identity.key.prev"),
            ]
        );
        assert!(lines(tmp.path()).iter().all(|l| l.starts_with("ERROR")));
    }
}
