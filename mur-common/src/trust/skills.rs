use crate::skill::ct_eq_hex;
use crate::skill::types::TrustLevel;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Current on-disk schema. Bump only for a change that needs a migration.
///
/// - **1** — hash keys written by `content_sha256` (plain canonical YAML).
/// - **2** — hash keys written by `content_hash_for_trust` (canonical YAML with
///   `transfer_chain` / `evolution_log` excluded), so a transfer or a
///   generation increment no longer re-keys an entry out from under the loader.
pub const TRUST_STORE_SCHEMA: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
pub struct SkillTrustStore {
    /// On-disk schema version. Absent in stores written before the field
    /// existed, which are exactly the v1 stores — hence `default = 1`.
    #[serde(default = "schema_v1")]
    pub schema: u32,

    pub entries: BTreeMap<String, TrustEntry>,

    /// Kill-switch — content hashes that may NEVER load, regardless of
    /// the per-entry trust level.
    #[serde(default)]
    pub revoked: Vec<String>,
}

fn schema_v1() -> u32 {
    1
}

impl Default for SkillTrustStore {
    /// A store created in memory is already current — only a store read from
    /// disk can be older, and `serde` supplies 1 for those.
    fn default() -> Self {
        Self {
            schema: TRUST_STORE_SCHEMA,
            entries: BTreeMap::new(),
            revoked: Vec::new(),
        }
    }
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

    /// Re-key v1 hash-keyed entries into the trust hash domain (schema 1 → 2).
    ///
    /// v1 keyed by `content_sha256`; the loader now looks up
    /// `content_hash_for_trust`. Without this every already-installed skill
    /// misses its entry and silently drops to `Sandboxed` — fail-closed, so no
    /// privilege is gained, but every recorded trust level would be lost.
    ///
    /// Re-keying needs the manifest, which the store does not hold, so each
    /// entry is recomputed from the skill still on disk. What that implies:
    ///
    /// - **Name-keyed entries are left alone.** `registry-add` keys by skill
    ///   name on purpose (the drift baseline). Only 64-hex keys are candidates.
    /// - **An entry whose skill is no longer installed is kept as-is.** It
    ///   cannot be recomputed, and dropping it would silently discard a
    ///   `Trusted` decision the user made. A stale key is inert; a deleted one
    ///   is not recoverable.
    /// - **Already-correct keys are cheap no-ops** — the recomputed hash equals
    ///   the existing key and the entry is reinserted unchanged.
    ///
    /// Returns `None` if the store was already current, or `Some(n)` with the
    /// number of entries re-keyed. `Some(0)` still means the schema was bumped
    /// and the store must be saved — otherwise a store with nothing to move
    /// never records that it migrated and repeats the work on every start.
    pub fn migrate_to_trust_hash<F>(&mut self, load_manifest: F) -> Option<usize>
    where
        F: Fn(&str) -> Option<crate::skill::SkillManifest>,
    {
        if self.schema >= TRUST_STORE_SCHEMA {
            return None;
        }
        let is_hash_key = |k: &str| k.len() == 64 && k.chars().all(|c| c.is_ascii_hexdigit());

        let mut rekeyed = 0usize;
        let mut moved: Vec<(String, String)> = Vec::new();
        for key in self.entries.keys() {
            if !is_hash_key(key) {
                continue; // name-keyed drift baseline — deliberately not a hash
            }
            let Some(entry) = self.entries.get(key) else {
                continue;
            };
            let Some(manifest) = load_manifest(&entry.name) else {
                continue; // skill gone from disk; keep the entry rather than lose it
            };
            let Ok(new_key) = crate::skill::content_hash_for_trust(&manifest) else {
                continue;
            };
            if new_key != *key {
                moved.push((key.clone(), new_key));
            }
        }
        for (old, new) in moved {
            if let Some(entry) = self.entries.remove(&old) {
                self.entries.insert(new, entry);
                rekeyed += 1;
            }
        }
        self.schema = TRUST_STORE_SCHEMA;
        Some(rekeyed)
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

    fn manifest(name: &str, evolved: bool) -> crate::skill::SkillManifest {
        let yaml = format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:t\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n"
        );
        let mut m = crate::skill::parse_canonical(&yaml).unwrap();
        if evolved {
            m.evolution_log
                .push(crate::skill::evolution::EvolutionEvent::initial_human(
                    "t", "1.0.0",
                ));
        }
        m
    }

    /// A store written before the schema field existed reads as v1 and migrates.
    #[test]
    fn a_store_without_a_schema_field_is_v1() {
        let s: SkillTrustStore = serde_json::from_str(r#"{"entries":{},"revoked":[]}"#).unwrap();
        assert_eq!(s.schema, 1);
        // ...while one built in memory is already current.
        assert_eq!(SkillTrustStore::default().schema, TRUST_STORE_SCHEMA);
    }

    /// Name-keyed entries are the drift baseline and must survive untouched —
    /// re-keying them to a hash would destroy the only record that spans
    /// versions.
    #[test]
    fn migration_leaves_name_keyed_entries_alone() {
        let m = manifest("demo", true);
        let mut s = SkillTrustStore {
            schema: 1,
            ..Default::default()
        };
        s.entries.insert("demo".into(), entry());

        let moved = s.migrate_to_trust_hash(|_| Some(m.clone()));

        assert_eq!(moved, Some(0), "a name key is not a hash key");
        assert!(s.entries.contains_key("demo"));
        assert_eq!(s.schema, TRUST_STORE_SCHEMA);
    }

    /// An entry whose skill is gone cannot be recomputed. Keep it: a stale key
    /// is inert, but discarding it would silently drop a trust decision.
    #[test]
    fn migration_keeps_an_entry_whose_skill_is_no_longer_installed() {
        let legacy = "a".repeat(64);
        let mut s = SkillTrustStore {
            schema: 1,
            ..Default::default()
        };
        s.entries.insert(legacy.clone(), entry());

        let moved = s.migrate_to_trust_hash(|_| None);

        assert_eq!(moved, Some(0));
        assert!(s.entries.contains_key(&legacy), "trust decision was lost");
    }

    /// The re-key itself, end to end, and the entry keeps its level.
    #[test]
    fn migration_rekeys_a_hash_entry_into_the_trust_domain() {
        let m = manifest("demo", true);
        let legacy = crate::skill::content_sha256(&m).unwrap();
        let target = crate::skill::content_hash_for_trust(&m).unwrap();
        assert_ne!(legacy, target, "precondition: domains must differ");

        let mut s = SkillTrustStore {
            schema: 1,
            ..Default::default()
        };
        s.entries.insert(legacy.clone(), entry());

        let moved = s.migrate_to_trust_hash(|_| Some(m.clone()));

        assert_eq!(moved, Some(1));
        assert!(!s.entries.contains_key(&legacy));
        assert_eq!(s.entries.get(&target).unwrap().level, TrustLevel::Verified);
    }

    /// Idempotent: a current store is left completely alone, and reports so.
    #[test]
    fn migration_is_a_no_op_on_a_current_store() {
        let m = manifest("demo", true);
        let mut s = SkillTrustStore::default();
        let legacy = crate::skill::content_sha256(&m).unwrap();
        s.entries.insert(legacy.clone(), entry());

        assert_eq!(s.migrate_to_trust_hash(|_| Some(m.clone())), None);
        assert!(
            s.entries.contains_key(&legacy),
            "a v2 store must not be re-keyed again"
        );
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
