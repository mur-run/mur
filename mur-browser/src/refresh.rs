//! Write a browser session's current storage state back to its profile.
//!
//! `mur browser auth` used to be the only writer of `state.json.age`, so
//! every replay and live session started from the login-time snapshot and
//! threw away whatever the site rotated afterwards (refreshed session
//! cookies, renewed web-storage tokens). On a site that rotates its token the
//! saved profile then went stale long before the account's session did.
//!
//! The browser exports its state into a private scratch directory under the
//! browser root (the sealed live server can write there and nowhere else),
//! and [`refresh_profile`] re-encrypts it — but only when the export still
//! carries every login key of the saved profile ([`session::retains_login`]);
//! leftover preferences or consent cookies are not proof of a login. A
//! logged-out or blank context must never overwrite a working login.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{auth, paths, session, state::StateKeyStore};

/// File name the browser writes its exported state to inside a scratch dir.
pub const EXPORT_FILE: &str = "storage-state.json";

/// How long a session-end export may take before it is abandoned; the
/// browser is about to be killed either way.
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// What happened to a profile after a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The profile now holds the session's latest state.
    Refreshed,
    /// Left untouched, with the reason (no export, logged out, …).
    Skipped(String),
}

/// `async (page) => …` snippet for `browser_run_code_unsafe` that writes the
/// context's storage state to `state_path`. The path is serialised as a JS
/// string literal, never interpolated raw.
pub fn export_code(state_path: &Path) -> Result<String> {
    let path = serde_json::to_string(&state_path.to_string_lossy())?;
    Ok(format!(
        "async (page) => {{ await page.context().storageState({{ path: {path} }}); return 'storage state saved'; }}"
    ))
}

/// Arguments for the `browser_run_code_unsafe` export call.
pub fn export_arguments(state_path: &Path) -> Result<serde_json::Value> {
    Ok(json!({ "code": export_code(state_path)? }))
}

/// Ask a live Playwright MCP server to export its state to `path`. Best
/// effort and bounded: a failure only means this session is not written back.
pub async fn export_via(down: &crate::proxy::Downstream, path: &Path) {
    let result = async {
        let call = down.call_tool("browser_run_code_unsafe", export_arguments(path)?);
        let resp = tokio::time::timeout(EXPORT_TIMEOUT, call)
            .await
            .context("storage-state export timed out")??;
        crate::proxy::ensure_tool_succeeded("browser_run_code_unsafe", &resp).map(|_| ())
    }
    .await;
    if let Err(error) = result {
        tracing::info!(error = %format!("{error:#}"), "session-end storage-state export skipped");
    }
}

/// A private (0700) scratch directory under the browser root, removed on
/// drop, for one session's state export.
pub fn scratch_dir(mur_home: &Path) -> Result<tempfile::TempDir> {
    let parent = paths::browser_root(mur_home).join("tmp");
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("create browser scratch dir {}", parent.display()))?;
    tempfile::Builder::new()
        .prefix("mur-browser-refresh-")
        .tempdir_in(&parent)
        .with_context(|| format!("create private export dir in {}", parent.display()))
}

/// Where the browser should write its export inside `dir`.
pub fn export_path(dir: &Path) -> PathBuf {
    dir.join(EXPORT_FILE)
}

/// Re-encrypt `site`'s profile from the export at `export`, if it is a
/// usable logged-in state. `last_auth` (the human login) is kept; the session
/// expiry and `last_refresh` are updated. The plaintext export is removed
/// whatever the outcome.
pub fn refresh_profile(
    mur_home: &Path,
    site: &str,
    export: &Path,
    now: DateTime<Utc>,
    store: &impl StateKeyStore,
) -> Result<Outcome> {
    let result = refresh_inner(mur_home, site, export, now, store);
    let _ = std::fs::remove_file(export);
    result
}

fn refresh_inner(
    mur_home: &Path,
    site: &str,
    export: &Path,
    now: DateTime<Utc>,
    store: &impl StateKeyStore,
) -> Result<Outcome> {
    if !export.is_file() {
        return Ok(Outcome::Skipped("the browser exported no state".into()));
    }
    let state = std::fs::read(export)
        .with_context(|| format!("read exported browser state {}", export.display()))?;
    let meta_path = paths::profile_meta(mur_home, site);
    let yaml = std::fs::read_to_string(&meta_path)
        .with_context(|| format!("read browser profile metadata {}", meta_path.display()))?;
    let mut meta: auth::ProfileMeta = serde_yaml::from_str(&yaml)
        .with_context(|| format!("parse browser profile metadata {}", meta_path.display()))?;
    auth::validate_storage_state(&state)?;
    let state_path = paths::profile_state(mur_home, site);
    let saved = crate::state::read_state(&state_path, store)
        .context("read the saved profile state to compare logins")?;
    let saved_at = meta.last_refresh.unwrap_or(meta.last_auth);
    if !session::retains_login(&saved, saved_at, &state, &meta.allow_domains, now) {
        return Ok(Outcome::Skipped(
            "the session no longer holds the saved login (logged out?)".into(),
        ));
    }
    crate::state::write_state(&state_path, &state, store)?;
    meta.earliest_cookie_expires =
        session::session_cookie_expiry(&state, &meta.allow_domains, now)?;
    meta.last_refresh = Some(now);
    auth::write_meta(&meta_path, &meta)?;
    Ok(Outcome::Refreshed)
}

/// Log a refresh result; a failed refresh never fails the session itself.
pub fn report(site: &str, result: Result<Outcome>) {
    match result {
        Ok(Outcome::Refreshed) => tracing::info!(site, "browser profile refreshed"),
        Ok(Outcome::Skipped(why)) => {
            tracing::info!(site, reason = %why, "browser profile not refreshed")
        }
        Err(error) => {
            tracing::warn!(site, error = %format!("{error:#}"), "browser profile refresh failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{read_state, write_state};

    #[derive(Default)]
    struct MemoryStore(std::sync::Mutex<std::collections::HashMap<String, String>>);
    impl StateKeyStore for MemoryStore {
        fn get(&self, account: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }
    }

    const OLD: &[u8] = br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":"old"}]}]}"#;

    fn setup(home: &Path, store: &MemoryStore) {
        std::fs::create_dir_all(paths::profile_dir(home, "ex")).unwrap();
        write_state(&paths::profile_state(home, "ex"), OLD, store).unwrap();
        let meta = auth::ProfileMeta {
            url: "https://chat.example.com/sign_in".into(),
            last_auth: DateTime::from_timestamp(1_600_000_000, 0).unwrap(),
            earliest_cookie_expires: DateTime::from_timestamp(1_600_001_800, 0),
            allow_domains: vec!["example.com".into()],
            last_refresh: None,
        };
        auth::write_meta(&paths::profile_meta(home, "ex"), &meta).unwrap();
    }

    fn meta(home: &Path) -> auth::ProfileMeta {
        serde_yaml::from_str(&std::fs::read_to_string(paths::profile_meta(home, "ex")).unwrap())
            .unwrap()
    }

    #[test]
    fn logged_in_export_replaces_the_profile_and_keeps_last_auth() {
        let home = tempfile::tempdir().unwrap();
        let store = MemoryStore::default();
        setup(home.path(), &store);
        let dir = scratch_dir(home.path()).unwrap();
        let export = export_path(dir.path());
        let new = br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":"new"}]}]}"#;
        std::fs::write(&export, new).unwrap();
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        let got = refresh_profile(home.path(), "ex", &export, now, &store).unwrap();

        assert_eq!(got, Outcome::Refreshed);
        assert_eq!(
            read_state(&paths::profile_state(home.path(), "ex"), &store).unwrap(),
            new
        );
        let m = meta(home.path());
        assert_eq!(
            m.last_auth,
            DateTime::from_timestamp(1_600_000_000, 0).unwrap()
        );
        assert_eq!(m.last_refresh, Some(now));
        // The stale 30-minute anti-bot expiry is gone: no session cookie.
        assert_eq!(m.earliest_cookie_expires, None);
        assert!(!export.exists(), "plaintext export must be removed");
    }

    #[test]
    fn logged_out_or_missing_export_never_overwrites_the_profile() {
        let home = tempfile::tempdir().unwrap();
        let store = MemoryStore::default();
        setup(home.path(), &store);
        let dir = scratch_dir(home.path()).unwrap();
        let export = export_path(dir.path());
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        let missing = refresh_profile(home.path(), "ex", &export, now, &store).unwrap();
        assert!(matches!(missing, Outcome::Skipped(_)));

        std::fs::write(
            &export,
            br#"{"cookies":[{"name":"__cf_bm","domain":".example.com"}],"origins":[]}"#,
        )
        .unwrap();
        let blank = refresh_profile(home.path(), "ex", &export, now, &store).unwrap();
        assert!(matches!(blank, Outcome::Skipped(_)));

        assert_eq!(
            read_state(&paths::profile_state(home.path(), "ex"), &store).unwrap(),
            OLD
        );
        assert_eq!(meta(home.path()).last_refresh, None);
        assert!(!export.exists());
    }

    #[test]
    fn logout_that_keeps_preferences_leaves_the_encrypted_profile_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let store = MemoryStore::default();
        setup(home.path(), &store);
        let before = std::fs::read(paths::profile_state(home.path(), "ex")).unwrap();
        let dir = scratch_dir(home.path()).unwrap();
        let export = export_path(dir.path());
        std::fs::write(
            &export,
            br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"theme","value":"dark"}]}]}"#,
        )
        .unwrap();
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        let got = refresh_profile(home.path(), "ex", &export, now, &store).unwrap();

        assert!(matches!(got, Outcome::Skipped(_)));
        assert_eq!(
            std::fs::read(paths::profile_state(home.path(), "ex")).unwrap(),
            before,
            "encrypted profile must be byte-for-byte unchanged"
        );
        assert_eq!(meta(home.path()).last_refresh, None);
        assert!(!export.exists());
    }

    #[test]
    fn logout_that_clears_the_token_value_leaves_the_encrypted_profile_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let store = MemoryStore::default();
        setup(home.path(), &store);
        let before = std::fs::read(paths::profile_state(home.path(), "ex")).unwrap();
        let dir = scratch_dir(home.path()).unwrap();
        let export = export_path(dir.path());
        // `localStorage.setItem("userToken", "")`: the key stays, the token is gone.
        std::fs::write(
            &export,
            br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":""},{"name":"theme","value":"dark"}]}]}"#,
        )
        .unwrap();
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        let got = refresh_profile(home.path(), "ex", &export, now, &store).unwrap();

        assert!(matches!(got, Outcome::Skipped(_)));
        assert_eq!(
            std::fs::read(paths::profile_state(home.path(), "ex")).unwrap(),
            before,
            "encrypted profile must be byte-for-byte unchanged"
        );
        assert_eq!(meta(home.path()).last_refresh, None);
        assert!(!export.exists());
    }

    #[test]
    fn export_code_quotes_hostile_paths() {
        let code = export_code(Path::new("/private/a\"b\\c/storage-state.json")).unwrap();
        assert!(
            code.contains("storageState({ path: \"/private/a\\\"b\\\\c/storage-state.json\" })")
        );
    }
}
