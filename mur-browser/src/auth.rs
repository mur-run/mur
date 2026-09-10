//! Authentication handoff state machine for headed browser profiles.
//!
//! The transport owns browser I/O; this module owns the narrow lifecycle
//! contract so it cannot save a profile before a human has explicitly taken
//! over and continued after login.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::{state, state::StateKeyStore};

/// One headed authentication session's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    AgentDriving,
    HumanDriving,
    Verifying,
    Saving,
    Done,
}

/// Metadata stored next to an encrypted browser profile. It intentionally
/// contains no credentials; expiry is informational only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub url: String,
    pub last_auth: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earliest_cookie_expires: Option<DateTime<Utc>>,
}

/// Persist a verified Playwright storage-state document and its non-secret
/// profile metadata. Metadata is only published after state encryption
/// succeeds, and the handoff reaches `Done` only after both files exist.
pub fn save_profile(
    handoff: &mut Handoff,
    state_path: &Path,
    meta_path: &Path,
    storage_state: &[u8],
    url: &str,
    now: DateTime<Utc>,
    store: &impl StateKeyStore,
) -> Result<ProfileMeta> {
    handoff.write_state(state_path, storage_state, store)?;
    let meta = ProfileMeta {
        url: url.to_owned(),
        last_auth: now,
        earliest_cookie_expires: earliest_cookie_expiry(storage_state)?,
    };
    write_meta(meta_path, &meta)?;
    handoff.saved()?;
    Ok(meta)
}

/// Atomically write non-secret profile metadata with owner-only permissions.
pub fn write_meta(path: &Path, meta: &ProfileMeta) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("profile metadata path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let yaml = serde_yaml::to_string(meta)?;
    let temp = parent.join(format!(".meta-{}.tmp", uuid::Uuid::new_v4().simple()));
    std::fs::write(&temp, yaml)?;
    #[cfg(unix)]
    std::fs::set_permissions(&temp, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })?;
    Ok(())
}

fn earliest_cookie_expiry(storage_state: &[u8]) -> Result<Option<DateTime<Utc>>> {
    let value: serde_json::Value = serde_json::from_slice(storage_state)?;
    let cookies = value
        .get("cookies")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("browser storage state must contain an array field \"cookies\"")
        })?;
    Ok(cookies
        .iter()
        .filter_map(|cookie| cookie.get("expires").and_then(serde_json::Value::as_f64))
        .filter(|expires| *expires > 0.0 && expires.is_finite())
        .filter_map(|expires| DateTime::from_timestamp(expires as i64, 0))
        .min())
}

/// State machine guarding an authentication handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handoff {
    phase: Phase,
}

impl Default for Handoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Handoff {
    pub fn new() -> Self {
        Self { phase: Phase::Idle }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Browser launch and initial navigation completed.
    pub fn start(&mut self) -> Result<()> {
        self.transition(Phase::Idle, Phase::AgentDriving, "start authentication")
    }

    /// The browser is exclusively controlled by the person signing in.
    pub fn handoff(&mut self) -> Result<()> {
        self.transition(Phase::AgentDriving, Phase::HumanDriving, "handoff to human")
    }

    /// The person confirmed that login is complete; only verification is next.
    pub fn continue_after_login(&mut self) -> Result<()> {
        self.transition(
            Phase::HumanDriving,
            Phase::Verifying,
            "continue after login",
        )
    }

    /// A verification snapshot proved the session is authenticated.
    pub fn verified(&mut self) -> Result<()> {
        self.transition(Phase::Verifying, Phase::Saving, "save authenticated state")
    }

    /// Verification did not prove login; return control to the person.
    pub fn verification_failed(&mut self) -> Result<()> {
        self.transition(Phase::Verifying, Phase::HumanDriving, "return to human")
    }

    /// The encrypted state and metadata were written successfully.
    pub fn saved(&mut self) -> Result<()> {
        self.transition(Phase::Saving, Phase::Done, "finish authentication")
    }

    /// Encrypt a verified Playwright storage-state JSON document without
    /// advancing the lifecycle; `save_profile` uses this before metadata.
    fn write_state(
        &mut self,
        path: &Path,
        storage_state: &[u8],
        store: &impl StateKeyStore,
    ) -> Result<()> {
        if self.phase != Phase::Saving {
            bail!(
                "cannot save authenticated state while authentication is {:?}; expected {:?}",
                self.phase,
                Phase::Saving
            );
        }
        validate_storage_state(storage_state)?;
        state::write_state(path, storage_state, store)
    }

    /// Persist a verified Playwright storage-state JSON document. This is the
    /// sole state-machine path that writes browser credentials to disk.
    pub fn save_state(
        &mut self,
        path: &Path,
        storage_state: &[u8],
        store: &impl StateKeyStore,
    ) -> Result<()> {
        self.write_state(path, storage_state, store)?;
        self.saved()
    }

    fn transition(&mut self, expected: Phase, next: Phase, action: &str) -> Result<()> {
        if self.phase != expected {
            bail!(
                "cannot {action} while authentication is {:?}; expected {:?}",
                self.phase,
                expected
            );
        }
        self.phase = next;
        Ok(())
    }
}

/// Validate the subset of Playwright's `storageState` schema that proves a
/// browser transport actually captured a storage-state document. Arbitrary
/// JSON (for example `{}`) must never become a reusable authenticated profile.
fn validate_storage_state(storage_state: &[u8]) -> Result<()> {
    let value: serde_json::Value = serde_json::from_slice(storage_state)
        .map_err(|error| anyhow::anyhow!("browser storage state is not valid JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("browser storage state must be a JSON object"))?;
    for field in ["cookies", "origins"] {
        if !object.get(field).is_some_and(serde_json::Value::is_array) {
            bail!("browser storage state must contain an array field {field:?}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_state_can_only_be_saved_after_human_handoff_and_verification() {
        let mut handoff = Handoff::new();
        assert!(handoff.saved().is_err());
        handoff.start().unwrap();
        assert!(handoff.continue_after_login().is_err());
        handoff.handoff().unwrap();
        handoff.continue_after_login().unwrap();
        handoff.verified().unwrap();
        handoff.saved().unwrap();
        assert_eq!(handoff.phase(), Phase::Done);
    }

    #[test]
    fn failed_verification_returns_to_human_control() {
        let mut handoff = Handoff::new();
        handoff.start().unwrap();
        handoff.handoff().unwrap();
        handoff.continue_after_login().unwrap();
        handoff.verification_failed().unwrap();
        assert_eq!(handoff.phase(), Phase::HumanDriving);
    }

    #[test]
    fn save_state_requires_verification_and_encrypts_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json.age");
        let store = MemoryStore::default();
        let mut handoff = Handoff::new();
        handoff.start().unwrap();
        handoff.handoff().unwrap();
        handoff.continue_after_login().unwrap();
        assert!(handoff.save_state(&path, br#"{}"#, &store).is_err());
        handoff.verified().unwrap();
        handoff
            .save_state(&path, br#"{"cookies":[],"origins":[]}"#, &store)
            .unwrap();
        assert_eq!(handoff.phase(), Phase::Done);
        assert_eq!(
            state::read_state(&path, &store).unwrap(),
            br#"{"cookies":[],"origins":[]}"#
        );
    }

    #[test]
    fn save_state_rejects_json_that_is_not_playwright_storage_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json.age");
        let store = MemoryStore::default();
        let mut handoff = Handoff::new();
        handoff.start().unwrap();
        handoff.handoff().unwrap();
        handoff.continue_after_login().unwrap();
        handoff.verified().unwrap();

        for invalid in [
            br#"[]"#.as_slice(),
            br#"{"cookies":[]}"#,
            br#"{"origins":{}}"#,
        ] {
            assert!(handoff.save_state(&path, invalid, &store).is_err());
            assert_eq!(handoff.phase(), Phase::Saving);
        }
    }

    #[test]
    fn save_profile_writes_non_secret_metadata_after_encryption() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json.age");
        let meta_path = dir.path().join("meta.yaml");
        let store = MemoryStore::default();
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut handoff = Handoff::new();
        handoff.start().unwrap();
        handoff.handoff().unwrap();
        handoff.continue_after_login().unwrap();
        handoff.verified().unwrap();
        let state = br#"{"cookies":[{"expires":1700100000}],"origins":[]}"#;

        let meta = save_profile(
            &mut handoff,
            &state_path,
            &meta_path,
            state,
            "https://example.test/login",
            now,
            &store,
        )
        .unwrap();

        assert_eq!(handoff.phase(), Phase::Done);
        assert_eq!(meta.url, "https://example.test/login");
        assert_eq!(
            meta.earliest_cookie_expires,
            DateTime::from_timestamp(1_700_100_000, 0)
        );
        let on_disk = std::fs::read_to_string(meta_path).unwrap();
        assert!(on_disk.contains("https://example.test/login"));
        assert!(!on_disk.contains("cookies"));
    }

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
}
