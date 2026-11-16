//! Letting the agent runtime read the browser state key without a prompt
//! (#2369).
//!
//! `mur browser auth` creates the key as the `mur` CLI, so only `mur` is on its
//! Keychain "Always Allow" list. The runtime reads the key unattended before
//! its sandbox seals; without being on that list it raises a prompt nobody
//! sees and times out. This adds the runtime installed beside `mur` to the
//! list: after every `mur browser auth` (which also migrates a key made by an
//! older MUR) and from `mur agent runtime-doctor --fix`.

use mur_common::agent_bin::RUNTIME_BIN;
use mur_common::browser_state_key::{KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE};
use mur_common::keychain_acl::{self, AclError, Trust};
use std::path::PathBuf;

/// The runtime binary beside the running `mur`, canonical. `None` when the
/// install dir cannot be resolved or holds no runtime.
fn runtime_path() -> Option<PathBuf> {
    let p = mur_common::mcp_first_party::current_install_dir()?.join(RUNTIME_BIN);
    p.canonicalize().ok()
}

/// What doctor reports about the key's ACL.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyAccess {
    /// No key yet, not macOS, or keychain access disabled: nothing to say.
    NotApplicable,
    /// The runtime reads the key without a prompt.
    Ok,
    /// The runtime is not trusted; every agent start raises a prompt.
    Missing(PathBuf),
    /// The check itself failed.
    Unknown(String),
}

/// Read-only check. Never prompts.
pub fn check() -> KeyAccess {
    let Some(runtime) = runtime_path() else {
        return KeyAccess::NotApplicable;
    };
    match keychain_acl::check(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, &runtime) {
        Ok(Trust::Trusted) => KeyAccess::Ok,
        Ok(Trust::Missing) => KeyAccess::Missing(runtime),
        Err(AclError::NotFound { .. } | AclError::Unsupported) => KeyAccess::NotApplicable,
        Err(e) => KeyAccess::Unknown(e.to_string()),
    }
}

/// Add the runtime to the key's ACL. macOS asks for the login keychain
/// password; run only from a command a person typed. `Ok(None)` when there is
/// nothing to do (no key, not macOS); `Ok(Some(changed))` otherwise.
pub fn grant() -> Result<Option<bool>, AclError> {
    let Some(runtime) = runtime_path() else {
        return Ok(None);
    };
    match keychain_acl::ensure(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, &runtime) {
        Ok(changed) => Ok(Some(changed)),
        Err(AclError::NotFound { .. } | AclError::Unsupported) => Ok(None),
        Err(e) => Err(e),
    }
}

/// After a profile is saved. Never fails the auth: the profile is already
/// written, and a missing grant only costs a prompt at the next agent start.
pub fn grant_after_auth(out: &mut dyn std::io::Write) {
    // No separate `check()` first: `grant` -> `keychain_acl::ensure` already
    // checks, and an already-trusted key comes back as `Ok(Some(false))`.
    let _ = match grant() {
        Ok(Some(true)) => writeln!(
            out,
            "agents can now read the browser key without a Keychain prompt"
        ),
        Ok(_) => Ok(()),
        Err(e) => writeln!(
            out,
            "warning: could not let agents read the browser key without a prompt ({e}); \
             run `mur agent runtime-doctor --fix` to try again"
        ),
    };
}

/// The runtime-doctor line, or `None` when all is well. With `fix`, grants
/// and reports the result instead of the hint.
pub fn doctor_line(fix: bool) -> Option<String> {
    match check() {
        KeyAccess::NotApplicable | KeyAccess::Ok => None,
        KeyAccess::Unknown(e) => Some(format!(
            "browser key: could not check its Keychain access ({e})"
        )),
        KeyAccess::Missing(runtime) if fix => Some(match grant() {
            Ok(Some(true)) => format!(
                "browser key: {} added to its Always Allow list — restart agents with a live \
                 browser (`mur agent restart <name>`)",
                runtime.display()
            ),
            Ok(_) => "browser key: nothing to change".to_string(),
            Err(e) => format!("browser key: could not update its Keychain access: {e}"),
        }),
        KeyAccess::Missing(runtime) => Some(format!(
            "browser key: {} is not on its Keychain Always Allow list, so every agent start \
             with a live browser raises a Keychain prompt and times out\n  fix with: mur agent \
             runtime-doctor --fix (asks for your login keychain password once)",
            runtime.display()
        )),
    }
}
