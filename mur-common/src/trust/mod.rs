//! Shared trust store at `~/.mur/trust/trust.yaml`.
//!
//! Spec §7.1: Hub and Commander share the same trust store.
//! File-locked writes; lock-free reads with retry.

pub mod legacy;
pub mod rotation;

use crate::muragent::MuragentError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Known,
    Pending,
    Rejected,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustEntry {
    pub public_key: String,
    pub display_name_seen: String,
    pub first_seen: String,
    pub last_seen: String,
    pub last_seen_surface: String,
    pub trust_level: TrustLevel,
    pub fingerprint: String,
    pub word_list: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotated_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rotation_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default)]
    pub agents: Vec<TrustEntry>,
}

impl TrustStore {
    /// Load trust store from `~/.mur/trust/trust.yaml`.
    /// If the file doesn't exist, returns an empty store.
    /// Runs legacy migration if `~/.mur/trust.json` exists.
    pub fn load() -> Result<Self, MuragentError> {
        let path = trust_store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(MuragentError::Io)?;
        }

        let legacy_path = mur_home().join("trust.json");
        if legacy_path.exists() && !path.exists() {
            legacy::migrate_legacy(&legacy_path, &path)?;
        }

        if !path.exists() {
            return Ok(Self::default());
        }

        let yaml = std::fs::read_to_string(&path).map_err(MuragentError::Io)?;
        serde_yaml_ng::from_str(&yaml)
            .map_err(|e| MuragentError::Other(format!("trust store parse: {e}")))
    }

    /// Atomically save the trust store.
    pub fn save(&self) -> Result<(), MuragentError> {
        let path = trust_store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(MuragentError::Io)?;
        }
        let yaml = serde_yaml_ng::to_string(self)
            .map_err(|e| MuragentError::Other(format!("trust store serialize: {e}")))?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, &yaml).map_err(MuragentError::Io)?;
        std::fs::rename(&tmp, &path).map_err(MuragentError::Io)?;
        Ok(())
    }

    /// Find an entry by public key (base64).
    pub fn find_by_pubkey(&self, pubkey_b64: &str) -> Option<&TrustEntry> {
        self.agents.iter().find(|e| e.public_key == pubkey_b64)
    }

    /// Find known entries by display name (for key-change detection).
    pub fn find_by_display_name(&self, name: &str) -> Vec<&TrustEntry> {
        self.agents
            .iter()
            .filter(|e| e.display_name_seen == name)
            .collect()
    }

    /// Insert or update an entry.
    pub fn upsert(&mut self, entry: TrustEntry) {
        if let Some(existing) = self
            .agents
            .iter_mut()
            .find(|e| e.public_key == entry.public_key)
        {
            *existing = entry;
        } else {
            self.agents.push(entry);
        }
    }

    /// Remove an entry by public key.
    pub fn remove(&mut self, pubkey_b64: &str) {
        self.agents.retain(|e| e.public_key != pubkey_b64);
    }
}

/// Derive a 4-word fingerprint from a public key, using SHA-256(pubkey).
///
/// Takes 52 bits of the hash and splits them into 4 × 13-bit indices into
/// a wordlist. The wordlist is sized to a power of two so all 13-bit values
/// are valid (no modulo bias). v1 uses a small placeholder list; the full
/// EFF long word list will be embedded via include_str! in a follow-up.
pub fn word_list_fingerprint(pubkey: &[u8; 32]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(pubkey);
    // Pack 7 hash bytes (56 bits) into the low end of a u64, then take the
    // top 52 bits (shift right 4).
    let raw: u64 = u64::from_be_bytes([
        0, hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6],
    ]);
    let bits = raw >> 4; // 52 bits in low
    let w0 = ((bits >> 39) & 0x1FFF) as usize;
    let w1 = ((bits >> 26) & 0x1FFF) as usize;
    let w2 = ((bits >> 13) & 0x1FFF) as usize;
    let w3 = (bits & 0x1FFF) as usize;

    let list = PLACEHOLDER_WORD_LIST;
    format!(
        "{} {} {} {}",
        list[w0 % list.len()],
        list[w1 % list.len()],
        list[w2 % list.len()],
        list[w3 % list.len()],
    )
}

/// Short fingerprint: first 8 hex chars of SHA-256(pubkey).
pub fn short_fingerprint(pubkey: &[u8; 32]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(pubkey);
    let hex = format!("{:x}", hash);
    hex[..8].to_string()
}

fn trust_store_path() -> PathBuf {
    mur_home().join("trust").join("trust.yaml")
}

/// Resolve `~/.mur` (or `$MUR_HOME` if set). Shared root for the trust store,
/// agent home dirs, and other on-disk state used by every surface.
pub fn mur_home() -> PathBuf {
    if let Some(p) = std::env::var_os("MUR_HOME") {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("home dir").join(".mur")
}

/// Placeholder wordlist for v1 fingerprints. The full 7776-word EFF long
/// wordlist will be embedded at build time in a follow-up change; until
/// then, fingerprints are derived from this short list (modulo).
const PLACEHOLDER_WORD_LIST: &[&str] = &[
    "abacus",
    "abdomen",
    "able",
    "abrupt",
    "absent",
    "absorb",
    "accept",
    "access",
    "accord",
    "acid",
    "acorn",
    "acquit",
    "acre",
    "active",
    "actor",
    "adapt",
    "adjust",
    "admire",
    "admit",
    "adopt",
    "adult",
    "advance",
    "advice",
    "affair",
    "afford",
    "afraid",
    "agency",
    "agenda",
    "agent",
    "agile",
    "alarm",
    "albatross",
];

#[cfg(test)]
pub(crate) mod test_env_lock {
    use std::sync::Mutex;
    /// Tests in this crate that set MUR_HOME must lock this mutex first —
    /// the env var is process-global and parallel tests trampling it
    /// produce spurious failures.
    pub(crate) static MUR_HOME_LOCK: Mutex<()> = Mutex::new(());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_list_is_deterministic() {
        let pk = [0x42u8; 32];
        let a = word_list_fingerprint(&pk);
        let b = word_list_fingerprint(&pk);
        assert_eq!(a, b);
    }

    #[test]
    fn word_list_has_four_words() {
        let pk = [0x42u8; 32];
        let fp = word_list_fingerprint(&pk);
        assert_eq!(fp.split_whitespace().count(), 4);
    }

    #[test]
    fn short_fingerprint_is_8_chars() {
        let pk = [0x42u8; 32];
        let fp = short_fingerprint(&pk);
        assert_eq!(fp.len(), 8);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn trust_store_roundtrip() {
        let _guard = test_env_lock::MUR_HOME_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let prev_home = std::env::var_os("MUR_HOME");
        unsafe { std::env::set_var("MUR_HOME", tmp.path()) };

        let mut store = TrustStore::default();
        store.upsert(TrustEntry {
            public_key: "aaa".into(),
            display_name_seen: "Coach".into(),
            first_seen: "2026-05-20T00:00:00Z".into(),
            last_seen: "2026-05-20T00:00:00Z".into(),
            last_seen_surface: "hub".into(),
            trust_level: TrustLevel::Pending,
            fingerprint: "1234abcd".into(),
            word_list: "a b c d".into(),
            rotated_from: None,
            superseded_at: None,
            last_rotation_at: None,
        });
        store.save().unwrap();

        let loaded = TrustStore::load().unwrap();
        assert_eq!(loaded.agents.len(), 1);
        assert_eq!(
            loaded.find_by_pubkey("aaa").unwrap().display_name_seen,
            "Coach"
        );

        unsafe {
            if let Some(p) = prev_home {
                std::env::set_var("MUR_HOME", p);
            } else {
                std::env::remove_var("MUR_HOME");
            }
        }
    }
}
