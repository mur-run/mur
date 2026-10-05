//! Session injection for recorded and live sessions.
//!
//! Replay decrypts a saved profile and hands Playwright
//! `--storage-state=<file>`; `mur browser record` took the same
//! `--profile <site>` flag and then ignored it, so every navigation arrived
//! with no cookies and the site bounced it to its login page. Granting a
//! branded Chrome.app does not change that — the state was never passed to
//! the launch at all.
//!
//! The decrypted copy lives in a 0600 temp file that is unlinked when the
//! returned guard drops, exactly as in `cmd::browser::replay`: Playwright
//! reads it once during launch, so it only has to outlive the spawn.

use anyhow::{Context, Result};
use mur_browser::{paths, state::KeychainStateKeyStore, state::read_state};
use std::path::Path;

/// A decrypted storage-state file, removed on drop.
#[derive(Debug)]
pub struct InjectedState(tempfile::NamedTempFile);

impl InjectedState {
    /// `--storage-state=<path>` for the launch.
    pub fn arg(&self) -> String {
        format!("--storage-state={}", self.0.path().display())
    }
}

/// Decrypt `site`'s saved state for a live launch. `None` for no profile —
/// a live session without one is legitimate (public pages), so this is not
/// an error. An unreadable profile IS an error: silently launching without a
/// session is what produced the login-page bounce this exists to fix.
pub fn prepare(mur_home: &Path, site: Option<&str>) -> Result<Option<InjectedState>> {
    let Some(site) = site else {
        return Ok(None);
    };
    let path = paths::profile_state(mur_home, site);
    // Check for the file before touching the Keychain: a profile that was never
    // saved is the common case, and decrypting first would make it an opaque
    // key-access failure (and, on a locked/headless Keychain, a hang).
    if !path.exists() {
        anyhow::bail!(
            "no saved browser profile {site:?} for live mode ({}) — run `mur browser auth {site} --url <login url>` first",
            path.display()
        );
    }
    let state = read_state(&path, &KeychainStateKeyStore).with_context(|| {
        format!(
            "decrypt browser profile {site:?} for live mode ({}) — run `mur browser auth {site} --url <login url>` first",
            path.display()
        )
    })?;
    // 0600 by construction, unlinked on drop.
    let mut file = tempfile::Builder::new()
        .prefix("mur-browser-live-state-")
        .suffix(".json")
        .tempfile()?;
    std::io::Write::write_all(&mut file, &state)?;
    drop(state);
    Ok(Some(InjectedState(file)))
}

/// Flags that already decide where the profile lives, so MUR must not add
/// `--isolated` on top: `--user-data-dir` is the persistent alternative and
/// the two are mutually exclusive.
const PROFILE_FLAGS: [&str; 2] = ["--isolated", "--user-data-dir"];

/// The launch args contributed by `state`: nothing without a profile,
/// otherwise the storage-state flag plus `--isolated` when nothing in `args`
/// has already chosen a profile mode.
///
/// `--isolated` is not optional decoration. `@playwright/mcp` documents
/// `--storage-state` as "the storage state file **for isolated sessions**":
/// in persistent mode the flag is ignored, so injecting without it silently
/// launches with no cookies — the login-page bounce all over again. Live mode
/// already passes `--isolated` itself; this is what makes test and automation
/// recording work against a logged-in area.
pub fn args(state: Option<&InjectedState>, args_so_far: &[String]) -> Vec<String> {
    let Some(state) = state else {
        return Vec::new();
    };
    let mut out = vec![state.arg()];
    if !args_so_far.iter().any(|a| {
        PROFILE_FLAGS
            .iter()
            .any(|f| a == f || a.starts_with(&format!("{f}=")))
    }) {
        out.push("--isolated".to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_profile_injects_nothing() {
        let home = tempfile::tempdir().unwrap();
        let state = prepare(home.path(), None).unwrap();
        assert!(state.is_none());
        assert!(args(state.as_ref(), &[]).is_empty());
    }

    /// A named profile that cannot be read must fail loudly: launching without
    /// the session is the bug, not the fallback.
    #[test]
    fn missing_profile_is_an_error_not_a_silent_skip() {
        let home = tempfile::tempdir().unwrap();
        let err = prepare(home.path(), Some("nope")).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("mur browser auth nope"), "{text}");
    }

    fn fake_state() -> InjectedState {
        InjectedState(tempfile::NamedTempFile::new().unwrap())
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|a| (*a).to_owned()).collect()
    }

    /// `--storage-state` is only honoured for isolated sessions, so an
    /// injection with no profile mode chosen must bring `--isolated` along.
    #[test]
    fn injection_adds_isolated_when_nothing_chose_a_profile_mode() {
        let state = fake_state();
        let out = args(Some(&state), &s(&["--headless"]));
        assert!(
            out.iter().any(|a| a.starts_with("--storage-state=")),
            "{out:?}"
        );
        assert!(out.contains(&"--isolated".to_owned()), "{out:?}");
    }

    #[test]
    fn injection_does_not_repeat_isolated() {
        let state = fake_state();
        let out = args(Some(&state), &s(&["--isolated"]));
        assert!(!out.contains(&"--isolated".to_owned()), "{out:?}");
    }

    /// `--user-data-dir` is the persistent alternative to `--isolated`;
    /// passing both makes @playwright/mcp refuse to launch.
    #[test]
    fn explicit_user_data_dir_suppresses_isolated() {
        let state = fake_state();
        for form in [
            s(&["--user-data-dir", "/tmp/p"]),
            s(&["--user-data-dir=/tmp/p"]),
        ] {
            let out = args(Some(&state), &form);
            assert!(!out.contains(&"--isolated".to_owned()), "{out:?}");
            assert_eq!(out.len(), 1, "{out:?}");
        }
    }
}
