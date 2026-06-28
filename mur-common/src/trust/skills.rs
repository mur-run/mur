use crate::skill::ct_eq_hex;
use crate::skill::types::TrustLevel;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SkillTrustStore {
    pub entries: BTreeMap<String, TrustEntry>,

    /// Kill-switch — content hashes that may NEVER load, regardless of
    /// the per-entry trust level.
    #[serde(default)]
    pub revoked: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustEntry {
    pub name: String,
    pub version: String,
    pub level: TrustLevel,
    pub installed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// SHA-256 hex of the installed skill YAML; used for rug-pull detection.
    #[serde(default)]
    pub content_sha256: String,
    /// Key fingerprint of the signer at install time; used for publisher-change detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key_fp: Option<String>,
}

#[derive(Debug)]
pub enum TrustStoreError {
    Io(io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for TrustStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustStoreError::Io(e) => write!(f, "io: {e}"),
            TrustStoreError::Parse(e) => write!(f, "parse: {e}"),
        }
    }
}

impl std::error::Error for TrustStoreError {}

impl From<io::Error> for TrustStoreError {
    fn from(e: io::Error) -> Self {
        TrustStoreError::Io(e)
    }
}

impl From<serde_json::Error> for TrustStoreError {
    fn from(e: serde_json::Error) -> Self {
        TrustStoreError::Parse(e)
    }
}

impl SkillTrustStore {
    pub fn path(mur_home: &Path) -> PathBuf {
        mur_home.join("trust").join("skills.json")
    }

    pub fn load(mur_home: &Path) -> Result<Self, TrustStoreError> {
        let p = Self::path(mur_home);
        if !p.exists() {
            return Ok(Self::default());
        }
        let s = fs::read_to_string(&p)?;
        if s.trim().is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_str(&s)?)
    }

    pub fn save(&self, mur_home: &Path) -> Result<(), TrustStoreError> {
        let dir = mur_home.join("trust");
        fs::create_dir_all(&dir)?;
        let lock_path = dir.join(".skills.lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock.lock_exclusive()?;

        let result = (|| -> Result<(), TrustStoreError> {
            let final_path = Self::path(mur_home);
            let tmp = dir.join(".skills.json.tmp");
            let json = serde_json::to_string_pretty(self)?;
            {
                let mut f = fs::File::create(&tmp)?;
                f.write_all(json.as_bytes())?;
                f.sync_all()?;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
            }
            fs::rename(&tmp, &final_path)?;
            Ok(())
        })();

        let _ = FileExt::unlock(&lock);
        let _ = lock;
        result
    }

    pub fn insert(&mut self, hash: String, entry: TrustEntry) {
        self.entries.insert(hash, entry);
    }

    pub fn lookup(&self, hash: &str) -> Option<&TrustEntry> {
        if self.is_revoked(hash) {
            return None;
        }
        for (k, v) in &self.entries {
            if ct_eq_hex(k, hash) {
                return Some(v);
            }
        }
        None
    }

    pub fn is_revoked(&self, hash: &str) -> bool {
        self.revoked.iter().any(|r| ct_eq_hex(r, hash))
    }

    pub fn revoke(&mut self, hash: &str) {
        if !self.is_revoked(hash) {
            self.revoked.push(hash.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn entry() -> TrustEntry {
        TrustEntry {
            name: "demo".into(),
            version: "1.0.0".into(),
            level: TrustLevel::Verified,
            installed_at: "2026-05-24T00:00:00Z".into(),
            publisher: Some("human:t".into()),
            ..Default::default()
        }
    }

    #[test]
    fn insert_lookup_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let mut s = SkillTrustStore::default();
        s.insert("a".repeat(64), entry());
        s.save(dir.path()).unwrap();
        let s2 = SkillTrustStore::load(dir.path()).unwrap();
        assert_eq!(s2.entries.len(), 1);
        assert_eq!(s2.lookup(&"a".repeat(64)).unwrap().name, "demo");
    }

    #[test]
    fn revoked_hash_returns_none() {
        let mut s = SkillTrustStore::default();
        let h = "b".repeat(64);
        s.insert(h.clone(), entry());
        s.revoke(&h);
        assert!(s.lookup(&h).is_none());
        assert!(s.is_revoked(&h));
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempdir().unwrap();
        let s = SkillTrustStore::load(dir.path()).unwrap();
        assert!(s.entries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let s = SkillTrustStore::default();
        s.save(dir.path()).unwrap();
        let mode = fs::metadata(SkillTrustStore::path(dir.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn revoke_is_idempotent() {
        let mut s = SkillTrustStore::default();
        s.revoke("c".repeat(64).as_str());
        s.revoke("c".repeat(64).as_str());
        assert_eq!(s.revoked.len(), 1);
    }
}
