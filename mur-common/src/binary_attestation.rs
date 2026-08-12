//! Binary attestation: verify that a spawned `mur-agent-runtime` carries a
//! valid signature from MUR's Developer ID team (launch-chain follow-on,
//! spec 2026-08-12). Gated to release builds by the build marker.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// True when this binary was built with `MUR_EMBED_RELEASE_MARKER=1`
/// (the release pipeline). Dev builds never verify.
pub const IS_EMBEDDED_RELEASE: bool = {
    // `==`/`match` on `&str` is not const-stable on current rustc, so compare
    // bytes (build.rs emits exactly "1" or "0").
    let bytes = env!("MUR_EMBEDDED_RELEASE").as_bytes();
    bytes.len() == 1 && bytes[0] == b'1'
};

/// MUR's Apple Developer Team ID (empty in dev builds; build.rs panics if the
/// marker is set without it).
pub const APPLE_TEAM_ID: &str = env!("MUR_APPLE_TEAM_ID");

/// The designated requirement used in production: valid signature chaining to
/// Apple plus a leaf certificate owned by MUR's team.
pub(crate) fn production_requirement() -> String {
    // Leading "=" marks the arg as literal requirement text to codesign (a
    // plain arg would be read as a file path); "designated =>" is accepted
    // only when *reading* a designated requirement, not as verification text.
    format!("=anchor apple generic and certificate leaf[subject.OU] = \"{APPLE_TEAM_ID}\"")
}

/// Verify `path` is a legitimate runtime binary. No-op unless this is a
/// macOS release build. Fail-closed: any verification error is returned.
pub fn verify_runtime_signature(path: &Path) -> Result<(), AttestError> {
    if !IS_EMBEDDED_RELEASE || !cfg!(target_os = "macos") {
        return Ok(());
    }
    // Canonicalize so a symlink or /var → /private/var redirect is verified
    // on the real file.
    let real = path.canonicalize().map_err(|e| AttestError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    verify_with_requirement(&real, &production_requirement())
}

/// Testable core: run `codesign --verify --strict -R <requirement>` on `path`.
#[doc(hidden)]
pub fn verify_with_requirement(path: &Path, requirement: &str) -> Result<(), AttestError> {
    let out = Command::new("codesign")
        .args(["--verify", "--strict", "-R", requirement])
        .arg(path)
        .output()
        .map_err(|e| AttestError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AttestError::VerificationFailed {
            path: path.to_path_buf(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}

#[derive(Debug)]
pub enum AttestError {
    /// The binary failed the designated requirement.
    VerificationFailed { path: PathBuf, stderr: String },
    /// Could not read/canonicalize the path or run codesign.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for AttestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VerificationFailed { path, stderr } => write!(
                f,
                "runtime binary at {} failed signature verification: {stderr}",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(
                    f,
                    "cannot verify runtime binary at {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for AttestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::VerificationFailed { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Compile-time gates: this binary is built without the release marker in
    // CI, so IS_EMBEDDED_RELEASE is false here — the skip behavior is the
    // negative control for every behavioral test below.
    #[test]
    #[allow(clippy::assertions_on_constants)] // runtime negative control on a compile-time const
    fn dev_build_never_verifies() {
        assert!(!IS_EMBEDDED_RELEASE);
        // verify_runtime_signature on a garbage path must still be Ok in dev:
        assert!(verify_runtime_signature(Path::new("/nonexistent/nope")).is_ok());
    }

    #[test]
    fn production_requirement_binds_anchor_and_team() {
        let req = production_requirement();
        assert!(req.contains("anchor apple generic"), "req: {req}");
        assert!(req.contains("subject.OU"), "req: {req}");
        assert!(req.starts_with("=anchor apple generic and"), "req: {req}");
    }

    // ── Behavioral matrix (macOS + test identity) ────────────────────────
    // These run only when MUR_TEST_SIGNING_OU is set (CI macOS job runs
    // scripts/test-signing-identity.sh first). Negative controls included.
    fn test_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mur-attest-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn test_ou() -> Option<String> {
        std::env::var("MUR_TEST_SIGNING_OU").ok()
    }

    #[test]
    #[cfg(unix)] // PermissionsExt::from_mode is unix-only (Windows CI compiles this module)
    fn unsigned_file_fails_test_requirement() {
        let Some(ou) = test_ou() else {
            eprintln!("skipping: MUR_TEST_SIGNING_OU not set");
            return;
        };
        let dir = test_dir("unsigned");
        let f = dir.join("runtime");
        std::fs::write(&f, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let req = format!("=certificate leaf[subject.OU] = \"{ou}\"");
        let err = verify_with_requirement(&f, &req).expect_err("unsigned must fail");
        assert!(matches!(err, AttestError::VerificationFailed { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)] // PermissionsExt::from_mode is unix-only (Windows CI compiles this module)
    fn adhoc_signed_fails_test_requirement() {
        let Some(ou) = test_ou() else {
            eprintln!("skipping");
            return;
        };
        let dir = test_dir("adhoc");
        let f = dir.join("runtime");
        std::fs::write(&f, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = std::process::Command::new("codesign")
            .args(["--force", "-s", "-"])
            .arg(&f)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ad-hoc sign failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let req = format!("=certificate leaf[subject.OU] = \"{ou}\"");
        let err = verify_with_requirement(&f, &req).expect_err("ad-hoc (no OU) must fail");
        assert!(matches!(err, AttestError::VerificationFailed { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)] // PermissionsExt::from_mode is unix-only (Windows CI compiles this module)
    fn wrong_ou_fails_test_requirement() {
        let Some(ou) = test_ou() else {
            eprintln!("skipping");
            return;
        };
        let dir = test_dir("wrongou");
        let f = dir.join("runtime");
        std::fs::write(&f, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = std::process::Command::new("codesign")
            .args(["--force", "-s", &format!("Mur Test ({ou})")])
            .arg(&f)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "sign failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let wrong = "=certificate leaf[subject.OU] = \"WRONGTEAM000\"".to_string();
        let err = verify_with_requirement(&f, &wrong).expect_err("wrong OU must fail");
        assert!(matches!(err, AttestError::VerificationFailed { .. }));
        // Positive control: the same signed binary passes with the right OU.
        let right = format!("=certificate leaf[subject.OU] = \"{ou}\"");
        verify_with_requirement(&f, &right).expect("matching OU must pass");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
}
