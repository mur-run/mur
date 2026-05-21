//! Install a validated `.muragent` archive onto the local host.
//!
//! Single source of truth for the `.muragent` install flow, shared by every
//! surface (CLI, Hub, Commander). The flow is:
//!
//! 1. Run the full 11-step validation pipeline (`validator::validate`).
//! 2. Validate the agent slug shape — prevents `agents/../../etc`.
//! 3. Check the trust store: a key change without a rotation manifest is a
//!    hard refuse (§7.1.1).
//! 4. Detect collision vs update by matching `agent.original_uuid` against
//!    any existing agent at the same slug. Same UUID → update (preserves
//!    `data/`); different UUID → error.
//! 5. Extract the payload to `<mur_home>/agents/<slug>/`.
//! 6. Upsert the trust store entry, marking surface and timestamps.
//!
//! UI/print decisions belong to the caller; this module returns a structured
//! `InstallOutcome` describing what happened.

use std::fs;
use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD as B64};

use crate::AgentProfile;
use crate::muragent::MuragentError;
use crate::muragent::manifest::MuragentManifest;
use crate::muragent::reader::MuragentArchive;
use crate::muragent::validator::{self, ValidationResult};
use crate::trust::{self, TrustEntry, TrustLevel, TrustStore};

/// Files in the .muragent that belong to the package envelope (not payload).
const ENVELOPE_FILES: &[&str] = &["manifest.yaml", "manifest.signed.json", "signatures.json"];

/// Result of a successful install or update.
pub struct InstallOutcome {
    pub manifest: MuragentManifest,
    pub trust_level: TrustLevel,
    pub fingerprint_hex: String,
    pub fingerprint_words: String,
    /// `false` when extracting into a freshly-created agent dir; `true` when
    /// the agent already existed at the slug with matching UUID and the
    /// payload was replaced in place (preserving `data/`).
    pub was_update: bool,
}

/// Install or update a `.muragent` archive. See module docs for the flow.
///
/// `mur_home` is the root for agent dirs (`<mur_home>/agents/<slug>/`). The
/// trust store is read and written via [`TrustStore::load`] / `save`, which
/// honour `$MUR_HOME` independently — callers should either pass the same
/// path the trust store would resolve, or set `MUR_HOME` consistently.
///
/// `surface` is recorded in the trust entry's `last_seen_surface` field.
/// Conventional values: `"cli"`, `"hub"`, `"commander"`.
pub fn install(
    archive: &MuragentArchive,
    mur_home: &Path,
    surface: &str,
) -> Result<InstallOutcome, MuragentError> {
    // Step 1: validation pipeline — fatal on any failure per §7.5
    let result = validator::validate(archive)?;

    // Step 2: slug shape
    let slug = result.manifest.agent.slug.clone();
    let display_name = result.manifest.agent.display_name.clone();
    crate::validate_agent_name(&slug).map_err(|e| {
        MuragentError::Other(format!("invalid agent slug '{slug}' in manifest: {e}"))
    })?;

    // Step 3: trust store key-change check
    let mut trust_store = TrustStore::load()?;
    let author_pubkey_b64 = B64.encode(result.author_pubkey);
    let existing_by_pubkey = trust_store.find_by_pubkey(&author_pubkey_b64).cloned();

    if existing_by_pubkey.is_none() {
        let by_name = trust_store.find_by_display_name(&display_name);
        if !by_name.is_empty() {
            return Err(MuragentError::TrustRefused(format!(
                "agent '{}' has a new signing key but no rotation manifest is present \
                 (possible impersonation; remove the existing trust entry first if intentional)",
                display_name
            )));
        }
    }

    // Step 4-5: detect update vs collision; extract payload
    let agent_dir = mur_home.join("agents").join(&slug);
    let was_update = if agent_dir.exists() {
        let existing_profile = agent_dir.join("profile.yaml");
        let mut is_same_agent = false;
        if existing_profile.exists() {
            let existing_yaml = fs::read_to_string(&existing_profile).map_err(MuragentError::Io)?;
            if let Ok(existing) = serde_yaml_ng::from_str::<AgentProfile>(&existing_yaml)
                && existing.id == result.manifest.agent.original_uuid
            {
                is_same_agent = true;
            }
        }
        if !is_same_agent {
            return Err(MuragentError::Other(format!(
                "agent '{slug}' already exists at {} with a different UUID",
                agent_dir.display()
            )));
        }
        // Same UUID — clear everything except data/, then extract
        clear_except_data(&agent_dir)?;
        true
    } else {
        fs::create_dir_all(&agent_dir).map_err(MuragentError::Io)?;
        false
    };

    extract_payload(archive, &agent_dir)?;

    // Step 6: trust upsert
    let fingerprint_hex = trust::short_fingerprint(&result.author_pubkey);
    let fingerprint_words = trust::word_list_fingerprint(&result.author_pubkey);
    let (trust_level, _) = upsert_trust(
        &mut trust_store,
        &result,
        &author_pubkey_b64,
        &existing_by_pubkey,
        surface,
    )?;
    trust_store.save()?;

    Ok(InstallOutcome {
        manifest: result.manifest,
        trust_level,
        fingerprint_hex,
        fingerprint_words,
        was_update,
    })
}

/// Remove every entry in `dir` except `data/`. Used by the update path.
fn clear_except_data(dir: &Path) -> Result<(), MuragentError> {
    for entry in fs::read_dir(dir).map_err(MuragentError::Io)? {
        let entry = entry.map_err(MuragentError::Io)?;
        if entry.file_name() == "data" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(MuragentError::Io)?;
        } else {
            fs::remove_file(&path).map_err(MuragentError::Io)?;
        }
    }
    Ok(())
}

fn extract_payload(archive: &MuragentArchive, agent_dir: &Path) -> Result<(), MuragentError> {
    for (path, data) in &archive.files {
        if ENVELOPE_FILES.contains(&path.as_str()) {
            continue;
        }
        let dest = agent_dir.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(MuragentError::Io)?;
        }
        fs::write(&dest, data).map_err(MuragentError::Io)?;
    }
    Ok(())
}

fn upsert_trust(
    trust_store: &mut TrustStore,
    result: &ValidationResult,
    author_pubkey_b64: &str,
    existing: &Option<TrustEntry>,
    surface: &str,
) -> Result<(TrustLevel, PathBuf), MuragentError> {
    let now = chrono::Utc::now().to_rfc3339();
    let first_seen = existing
        .as_ref()
        .map(|e| e.first_seen.clone())
        .unwrap_or_else(|| now.clone());
    // Promotion to Known is a UI decision, not an install-flow decision.
    // First-time-seen authors land at Pending and stay there until the
    // surface explicitly promotes them.
    let level = existing
        .as_ref()
        .map(|e| e.trust_level.clone())
        .unwrap_or(TrustLevel::Pending);

    trust_store.upsert(TrustEntry {
        public_key: author_pubkey_b64.to_string(),
        display_name_seen: result.manifest.agent.display_name.clone(),
        first_seen,
        last_seen: now,
        last_seen_surface: surface.to_string(),
        trust_level: level.clone(),
        fingerprint: trust::short_fingerprint(&result.author_pubkey),
        word_list: trust::word_list_fingerprint(&result.author_pubkey),
        rotated_from: existing.as_ref().and_then(|e| e.rotated_from.clone()),
        superseded_at: existing.as_ref().and_then(|e| e.superseded_at.clone()),
        last_rotation_at: existing.as_ref().and_then(|e| e.last_rotation_at.clone()),
    });

    Ok((level, PathBuf::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentIdentity;
    use crate::muragent::writer::{MuragentWriter, build_manifest_from_profile};
    use tempfile::TempDir;

    fn make_test_package(tmp: &TempDir) -> std::path::PathBuf {
        let out = tmp.path().join("test.muragent");
        let profile = AgentProfile::default_for_tests();
        let identity = AgentIdentity::generate();
        let manifest = build_manifest_from_profile(&profile, "2.13.0");
        let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
        let mut writer = MuragentWriter::new(manifest, profile_yaml, identity);
        writer.add_icon("icon-512.png", b"fake-png".to_vec());
        writer.write(&out).unwrap();
        out
    }

    #[test]
    fn install_then_update_preserves_data() {
        let _guard = crate::trust::test_env_lock::MUR_HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path().join("mur");
        let prev = std::env::var_os("MUR_HOME");
        unsafe { std::env::set_var("MUR_HOME", &mur_home) };

        let pkg = make_test_package(&tmp);
        let archive = MuragentArchive::read(&pkg).unwrap();
        let outcome = install(&archive, &mur_home, "test").unwrap();
        assert!(!outcome.was_update);
        let slug = outcome.manifest.agent.slug.clone();
        let agent_dir = mur_home.join("agents").join(&slug);
        assert!(agent_dir.join("profile.yaml").exists());

        // Caller writes some data — the update path must preserve it.
        let data_dir = agent_dir.join("data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join("history.jsonl"), b"important").unwrap();

        // Re-install (same archive, same UUID) — should preserve data/
        let outcome2 = install(&archive, &mur_home, "test").unwrap();
        assert!(outcome2.was_update);
        let preserved = fs::read(data_dir.join("history.jsonl")).unwrap();
        assert_eq!(preserved, b"important");

        // Cleanup
        unsafe {
            if let Some(p) = prev {
                std::env::set_var("MUR_HOME", p);
            } else {
                std::env::remove_var("MUR_HOME");
            }
        }
    }
}
