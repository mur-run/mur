//! Adding an app to a macOS Keychain item's "always allow" list (#2369).
//!
//! An item only trusts the binary that created it. Any other binary that
//! reads it gets a modal "allow access" prompt. The agent runtime reads the
//! browser state key unattended, just before its sandbox seals, so a prompt
//! there blocks until the read times out. This module adds the runtime to the
//! item's trusted-application list up front, the same way `-T /usr/bin/codesign`
//! pre-authorises codesign for the self-signed certificate
//! (`mur-core` `doctor_codesign`).
//!
//! Only the legacy `SecAccess` / `SecACL` calls can edit that list. Apple marks
//! them deprecated, but they are still the only API for file-based keychains
//! (`login.keychain-db`), and `security-framework` does not wrap them.
//!
//! [`check`] never prompts: it reads the item's access record, not its secret.
//! [`ensure`] changes the access record, which macOS guards with the login
//! keychain password. Call it only from a command a person is running.

use std::path::Path;

/// Whether an item's decrypt ACL already lets an app read it without a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The app is listed, or the ACL has no app list (any app may read).
    Trusted,
    /// The app is not listed, so a read raises a prompt.
    Missing,
}

#[derive(Debug, thiserror::Error)]
pub enum AclError {
    #[error("keychain item {service}/{account} does not exist")]
    NotFound { service: String, account: String },
    #[error("{call} failed (OSStatus {status})")]
    Os { call: &'static str, status: i32 },
    #[error("keychain item has no app list to add to")]
    NoAppList,
    #[error("keychain ACLs are only managed on macOS")]
    Unsupported,
}

/// What [`check`] decides from the decrypt ACLs it found. `None` is an ACL with
/// no app list; `Some` holds each listed app's identifying bytes.
#[cfg(any(target_os = "macos", test))]
fn decide(acls: &[Option<Vec<Vec<u8>>>], target: &[u8]) -> Trust {
    let target = trim_nul(target);
    let listed = acls.iter().any(|acl| match acl {
        None => true,
        Some(apps) => apps.iter().any(|a| trim_nul(a) == target),
    });
    if listed {
        Trust::Trusted
    } else {
        Trust::Missing
    }
}

/// `SecTrustedApplicationCopyData` returns a C path, sometimes with its NUL.
#[cfg(any(target_os = "macos", test))]
fn trim_nul(b: &[u8]) -> &[u8] {
    let end = b.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1);
    &b[..end]
}

/// Whether `app` can read the generic password `service`/`account` without a
/// prompt. Never prompts.
pub fn check(service: &str, account: &str, app: &Path) -> Result<Trust, AclError> {
    if crate::secret::keychain_blocked() {
        return Err(AclError::Unsupported);
    }
    imp::check(service, account, app)
}

/// Add `app` to the item's decrypt ACL. Returns `true` when the ACL changed,
/// `false` when `app` was already trusted. macOS asks for the login keychain
/// password the first time; run it only where a person can answer.
pub fn ensure(service: &str, account: &str, app: &Path) -> Result<bool, AclError> {
    if crate::secret::keychain_blocked() {
        return Err(AclError::Unsupported);
    }
    imp::ensure(service, account, app)
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    pub(super) fn check(_: &str, _: &str, _: &Path) -> Result<Trust, AclError> {
        Err(AclError::Unsupported)
    }
    pub(super) fn ensure(_: &str, _: &str, _: &Path) -> Result<bool, AclError> {
        Err(AclError::Unsupported)
    }
}

#[cfg(target_os = "macos")]
#[path = "keychain_acl_macos.rs"]
mod imp;

#[cfg(test)]
mod tests {
    use super::*;

    fn apps(list: &[&str]) -> Option<Vec<Vec<u8>>> {
        Some(list.iter().map(|s| s.as_bytes().to_vec()).collect())
    }

    #[test]
    fn a_listed_app_is_trusted_with_or_without_the_nul() {
        let acls = [apps(&["/bin/mur\0", "/usr/bin/security"])];
        assert_eq!(decide(&acls, b"/bin/mur"), Trust::Trusted);
        assert_eq!(decide(&acls, b"/bin/mur\0"), Trust::Trusted);
    }

    #[test]
    fn an_unlisted_app_is_missing() {
        let acls = [apps(&["/bin/mur"])];
        assert_eq!(decide(&acls, b"/bin/mur-agent-runtime"), Trust::Missing);
        assert_eq!(decide(&[], b"/bin/mur"), Trust::Missing);
    }

    #[test]
    fn an_acl_without_an_app_list_lets_anyone_read() {
        assert_eq!(decide(&[apps(&["/x"]), None], b"/y"), Trust::Trusted);
    }

    #[test]
    fn trim_only_drops_trailing_nuls() {
        assert_eq!(trim_nul(b"a\0b\0\0"), b"a\0b");
        assert_eq!(trim_nul(b"\0"), b"");
    }

    #[test]
    fn blocked_keychain_never_touches_the_os() {
        // nextest sets NEXTEST, which blocks the keychain.
        if crate::secret::keychain_blocked() {
            assert!(matches!(
                check("s", "a", Path::new("/x")),
                Err(AclError::Unsupported)
            ));
            assert!(matches!(
                ensure("s", "a", Path::new("/x")),
                Err(AclError::Unsupported)
            ));
        }
    }
}
