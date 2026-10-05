//! Session injection for live mode.
//!
//! Replay decrypts a saved profile and hands Playwright
//! `--storage-state=<file>`; live mode took the same `--profile <site>` flag
//! and then ignored it, so every live navigation arrived with no cookies and
//! the site bounced it to its login page. Granting a branded Chrome.app does
//! not change that — the state was never passed to the launch at all.
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

/// The launch args contributed by `state`: the storage-state flag, or nothing.
pub fn args(state: Option<&InjectedState>) -> Vec<String> {
    state.map(|s| vec![s.arg()]).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_profile_injects_nothing() {
        let home = tempfile::tempdir().unwrap();
        let state = prepare(home.path(), None).unwrap();
        assert!(state.is_none());
        assert!(args(state.as_ref()).is_empty());
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
}
