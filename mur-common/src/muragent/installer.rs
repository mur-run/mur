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
use crate::trust::rotation::RotationManifest;
use crate::trust::{self, TrustEntry, TrustLevel, TrustStore};

/// Files in the .muragent that belong to the package envelope (not payload).
const ENVELOPE_FILES: &[&str] = &["manifest.yaml", "manifest.signed.json", "signatures.json"];

/// Host-local files a `.muragent` must never carry. `identity.key` is the
/// agent's *private* signing key, minted locally and stripped by export; a
/// package that ships one of these is malformed or hostile, since extracting it
/// would plant or overwrite the agent's identity (impersonation, or breaking
/// re-export). Matched against the top-level extraction path.
const RESERVED_LOCAL_FILES: &[&str] = &[
    "identity.key",
    "identity.pub",
    "identity.key.prev",
    "identity.pub.prev",
];

/// Result of a successful install or update.
#[derive(Debug)]
pub struct InstallOutcome {
    pub manifest: MuragentManifest,
    pub trust_level: TrustLevel,
    pub fingerprint_hex: String,
    pub fingerprint_words: String,
    /// `false` when extracting into a freshly-created agent dir; `true` when
    /// the agent already existed at the slug with matching UUID and the
    /// payload was replaced in place (preserving `data/`).
    pub was_update: bool,
    /// Revocation outcome for this install. A *revoked* key/package never
    /// reaches here (it is a hard error); this is `Clean`/`Stale`/`Unknown` so
    /// the caller can warn or gate per its own policy.
    pub revocation_status: trust::RevocationStatus,
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

    // Step 1.5: reject host-local identity material in the payload. Extraction
    // writes every payload file into the agent dir, so a package shipping
    // `identity.key` would plant/overwrite the agent's private signing key.
    reject_reserved_local_files(archive)?;

    // Load the trust store up front: it carries the highest revocations
    // `crl_number` we've accepted, which the revocation check needs to reject a
    // rolled-back cache.
    let mut trust_store = TrustStore::load()?;

    // Step 1.6: revocation check (§7.4.1). The validator defers this; enforce it
    // here against the locally-cached list. A missing cache is fail-open (v1
    // best-effort, matching offline installs), but a cached list that names this
    // author key or this package hash is a hard refusal — a compromised author
    // key must not be re-accepted on import. The non-revoked status is surfaced
    // to the caller for policy.
    let revocation_status = check_revocations(
        archive,
        mur_home,
        &result.author_pubkey,
        trust_store.revocation_crl_number,
    )?;
    // Advance the persisted high-water mark so a later rollback is rejected.
    if let trust::RevocationStatus::Clean { crl_number }
    | trust::RevocationStatus::Stale { crl_number } = revocation_status
    {
        let hi = trust_store
            .revocation_crl_number
            .unwrap_or(0)
            .max(crl_number);
        trust_store.revocation_crl_number = Some(hi);
    }

    // Step 2: slug shape
    let slug = result.manifest.agent.slug.clone();
    let display_name = result.manifest.agent.display_name.clone();
    crate::validate_agent_name(&slug).map_err(|e| {
        MuragentError::Other(format!("invalid agent slug '{slug}' in manifest: {e}"))
    })?;

    // Step 3: trust store key-change check
    let author_pubkey_b64 = B64.encode(result.author_pubkey);
    let existing_by_pubkey = trust_store.find_by_pubkey(&author_pubkey_b64).cloned();

    if existing_by_pubkey.is_none() {
        let by_name = trust_store.find_by_display_name(&display_name);
        if !by_name.is_empty() {
            // Key change detected — look for a rotation manifest before refusing.
            let old_entry = by_name
                .into_iter()
                .find(|e| e.trust_level != TrustLevel::Superseded)
                .cloned();
            match try_apply_rotation(
                &mut trust_store,
                old_entry.as_ref(),
                &author_pubkey_b64,
                &display_name,
                mur_home,
            ) {
                Ok(()) => {} // rotation accepted; trust store updated in-place
                Err(reason) => {
                    return Err(MuragentError::TrustRefused(format!(
                        "agent '{}' has a new signing key but no valid rotation manifest: {}",
                        display_name, reason
                    )));
                }
            }
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
        revocation_status,
    })
}

/// Check an install against the locally-cached revocations list.
///
/// A revoked author key or package hash is a hard `TrustRefused` error. The
/// non-revoked outcomes are returned as a [`RevocationStatus`] so the caller can
/// apply its own policy to "unknown"/"stale" (v1 fail-open with a signal; a
/// governance surface may fail-closed). No network fetch happens here.
///
/// `known_crl` is the highest `crl_number` this host has already accepted; a
/// cached list older than that is a rollback (or tamper) and is ignored
/// (→ `Unknown`) rather than trusted, so it can neither downgrade a "clean"
/// verdict nor suppress a revocation we already knew about.
fn check_revocations(
    archive: &MuragentArchive,
    mur_home: &Path,
    author_pubkey: &[u8; 32],
    known_crl: Option<u64>,
) -> Result<trust::RevocationStatus, MuragentError> {
    use trust::RevocationStatus;

    let Some(list) = trust::RevocationsList::load_cached(mur_home) else {
        return Ok(RevocationStatus::Unknown);
    };

    // Rollback defence: never act on a cache older than what we have recorded.
    if let Some(known) = known_crl
        && list.crl_number < known
    {
        return Ok(RevocationStatus::Unknown);
    }

    let author = format!("ed25519:{}", B64.encode(author_pubkey));
    if list.is_author_revoked(&author) {
        return Err(MuragentError::TrustRefused(format!(
            "author key {author} is revoked"
        )));
    }

    if let Some(signed_json) = archive.get("manifest.signed.json") {
        use sha2::Digest;
        let manifest_hash = format!("sha256:{}", hex::encode(sha2::Sha256::digest(signed_json)));
        if list.is_package_revoked(&manifest_hash) {
            return Err(MuragentError::TrustRefused(format!(
                "package {manifest_hash} is revoked"
            )));
        }
    }

    Ok(if list.is_expired() {
        RevocationStatus::Stale {
            crl_number: list.crl_number,
        }
    } else {
        RevocationStatus::Clean {
            crl_number: list.crl_number,
        }
    })
}

/// Refuse a package that carries any [`RESERVED_LOCAL_FILES`] entry at its top
/// level — extracting it would plant/overwrite host-minted identity material.
fn reject_reserved_local_files(archive: &MuragentArchive) -> Result<(), MuragentError> {
    for path in archive.files.keys() {
        if RESERVED_LOCAL_FILES.contains(&path.as_str()) {
            return Err(MuragentError::Other(format!(
                "package contains reserved local file '{path}' \
                 (private identity material is host-minted and must not be shipped)"
            )));
        }
    }
    Ok(())
}

/// Convert a display name to a filesystem-safe slug for rotation manifest lookup.
fn display_name_slug(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn rotation_manifest_path(mur_home: &Path, display_name: &str) -> PathBuf {
    mur_home
        .join("trust")
        .join("rotations")
        .join(format!("{}.yaml", display_name_slug(display_name)))
}

/// Try to load and apply a key rotation manifest. Returns Ok(()) if the
/// rotation is valid and the trust store has been updated in-place. Returns
/// Err(reason) if the manifest is missing, invalid, or replayed.
fn try_apply_rotation(
    trust_store: &mut TrustStore,
    old_entry: Option<&TrustEntry>,
    new_pubkey_b64: &str,
    display_name: &str,
    mur_home: &Path,
) -> Result<(), String> {
    // Revocation takes precedence over rotation. A revoked old key must not be
    // able to authorize a rotation — otherwise an attacker holding a compromised
    // (and therefore revoked) key could mint a rotation to a key they control
    // and hijack the agent's identity. Checked before anything else so a revoked
    // key fails fast, regardless of whether a rotation manifest is present.
    if let Some(entry) = old_entry
        && let Some(list) = trust::RevocationsList::load_cached(mur_home)
        && list.is_author_revoked(&format!("ed25519:{}", entry.public_key))
    {
        return Err("previous signing key is revoked; rotation refused".into());
    }

    let manifest_path = rotation_manifest_path(mur_home, display_name);
    if !manifest_path.exists() {
        return Err(
            "no rotation manifest is present (possible impersonation; place \
             <display_name>.yaml in ~/.mur/trust/rotations/ if intentional)"
                .into(),
        );
    }

    let yaml =
        fs::read_to_string(&manifest_path).map_err(|e| format!("read rotation manifest: {e}"))?;
    let manifest: RotationManifest =
        serde_yaml_ng::from_str(&yaml).map_err(|e| format!("parse rotation manifest: {e}"))?;

    // Cross-check: manifest must reference the known old key and the incoming new key.
    if let Some(entry) = old_entry
        && manifest.old_pubkey != entry.public_key
    {
        return Err("rotation manifest old_pubkey does not match the known trust entry".into());
    }
    if manifest.new_pubkey != new_pubkey_b64 {
        return Err("rotation manifest new_pubkey does not match the package's signing key".into());
    }

    // Cryptographic verification (old key signs, new key countersigns).
    manifest.verify()?;

    // Replay prevention: issued_at must be strictly newer than last_rotation_at.
    // Compare parsed instants, not raw strings — RFC3339 is not lexicographically
    // ordered across offsets/precision ("Z" vs "+00:00", fractional seconds), so a
    // string compare could let a replayed manifest slip through.
    if let Some(entry) = old_entry
        && let Some(last_at) = &entry.last_rotation_at
    {
        let issued = chrono::DateTime::parse_from_rfc3339(&manifest.issued_at)
            .map_err(|e| format!("rotation manifest issued_at is not valid RFC3339: {e}"))?;
        let last = chrono::DateTime::parse_from_rfc3339(last_at)
            .map_err(|e| format!("stored last_rotation_at is not valid RFC3339: {e}"))?;
        if issued <= last {
            return Err(format!(
                "rotation manifest issued_at ({}) is not newer than last_rotation_at ({})",
                manifest.issued_at, last_at
            ));
        }
    }

    // Apply: mark old entry Superseded, insert new entry.
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(entry) = old_entry.cloned() {
        trust_store.upsert(TrustEntry {
            trust_level: TrustLevel::Superseded,
            superseded_at: Some(manifest.issued_at.clone()),
            last_rotation_at: Some(manifest.issued_at.clone()),
            ..entry
        });
    }
    trust_store.upsert(TrustEntry {
        public_key: new_pubkey_b64.to_string(),
        display_name_seen: display_name.to_string(),
        first_seen: now.clone(),
        last_seen: now,
        last_seen_surface: String::new(), // filled by caller during upsert_trust
        trust_level: TrustLevel::Pending,
        fingerprint: String::new(), // filled by caller
        word_list: String::new(),   // filled by caller
        rotated_from: old_entry.map(|e| e.public_key.clone()),
        superseded_at: None,
        last_rotation_at: Some(manifest.issued_at.clone()),
    });

    Ok(())
}

/// Files and directories that must survive an in-place update: the agent's
/// runtime `data/` and its local identity keypair (private material the
/// incoming, sanitized package never carries — clobbering it would orphan the
/// agent's signing key and break re-export).
const PRESERVE_ON_UPDATE: &[&str] = &[
    "data",
    "identity.key",
    "identity.pub",
    "identity.key.prev",
    "identity.pub.prev",
];

/// Remove every entry in `dir` except the [`PRESERVE_ON_UPDATE`] set. Used by
/// the update path.
fn clear_except_data(dir: &Path) -> Result<(), MuragentError> {
    for entry in fs::read_dir(dir).map_err(MuragentError::Io)? {
        let entry = entry.map_err(MuragentError::Io)?;
        if PRESERVE_ON_UPDATE
            .iter()
            .any(|keep| entry.file_name() == *keep)
        {
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
        if ENVELOPE_FILES.contains(&path.as_str()) || RESERVED_LOCAL_FILES.contains(&path.as_str())
        {
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
    fn install_extracts_sys_prompt_and_skills() {
        // Regression: `.muragent` export must bundle the system prompt and
        // skill files so the loaded agent keeps its persona + non-dangling
        // skill registrations.
        let _guard = crate::trust::test_env_lock::MUR_HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path().join("mur");
        let prev = std::env::var_os("MUR_HOME");
        unsafe { std::env::set_var("MUR_HOME", &mur_home) };

        let profile = AgentProfile::default_for_tests();
        let manifest = build_manifest_from_profile(&profile, "2.13.0");
        let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
        let mut writer = MuragentWriter::new(manifest, profile_yaml, AgentIdentity::generate());
        writer.set_sys_prompt("You are a helpful test agent.".into());
        writer.add_skill("demo.md", b"# demo skill\nbody".to_vec());
        let out = tmp.path().join("withextras.muragent");
        writer.write(&out).unwrap();

        let archive = MuragentArchive::read(&out).unwrap();
        let outcome = install(&archive, &mur_home, "test").unwrap();
        let agent_dir = mur_home.join("agents").join(&outcome.manifest.agent.slug);

        let prompt = fs::read_to_string(agent_dir.join("sys_prompt.md")).unwrap();
        assert_eq!(prompt, "You are a helpful test agent.");
        let skill = fs::read_to_string(agent_dir.join("skills").join("demo.md")).unwrap();
        assert_eq!(skill, "# demo skill\nbody");

        unsafe {
            if let Some(p) = prev {
                std::env::set_var("MUR_HOME", p);
            } else {
                std::env::remove_var("MUR_HOME");
            }
        }
    }

    fn make_test_package_with_identity(
        tmp: &TempDir,
        identity: &AgentIdentity,
    ) -> std::path::PathBuf {
        let out = tmp
            .path()
            .join(format!("{}.muragent", &identity.pubkey_text()[..8]));
        let profile = AgentProfile::default_for_tests();
        let manifest = build_manifest_from_profile(&profile, "2.13.0");
        let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
        let mut writer = MuragentWriter::new(manifest, profile_yaml, identity.clone());
        writer.add_icon("icon-512.png", b"fake-png".to_vec());
        writer.write(&out).unwrap();
        out
    }

    #[test]
    fn rotation_manifest_missing_still_refuses() {
        let _guard = crate::trust::test_env_lock::MUR_HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path().join("mur");
        let prev = std::env::var_os("MUR_HOME");
        unsafe { std::env::set_var("MUR_HOME", &mur_home) };

        let old_identity = AgentIdentity::generate();
        let pkg_old = make_test_package_with_identity(&tmp, &old_identity);
        let archive = MuragentArchive::read(&pkg_old).unwrap();
        let outcome = install(&archive, &mur_home, "test").unwrap();
        let slug = outcome.manifest.agent.slug.clone();

        let new_identity = AgentIdentity::generate();
        let profile = AgentProfile::default_for_tests();
        let out2 = tmp.path().join("new2.muragent");
        let manifest2 = build_manifest_from_profile(&profile, "2.14.0");
        let profile_yaml2 = serde_yaml_ng::to_string(&profile).unwrap();
        let mut writer2 = MuragentWriter::new(manifest2, profile_yaml2, new_identity);
        writer2.add_icon("icon-512.png", b"fake-png".to_vec());
        writer2.write(&out2).unwrap();
        let archive2 = MuragentArchive::read(&out2).unwrap();
        let agent_dir = mur_home.join("agents").join(&slug);
        fs::remove_dir_all(&agent_dir).unwrap();

        let err = install(&archive2, &mur_home, "test").unwrap_err();
        assert!(
            matches!(err, MuragentError::TrustRefused(_)),
            "expected TrustRefused, got: {:?}",
            err
        );

        unsafe {
            if let Some(p) = prev {
                std::env::set_var("MUR_HOME", p);
            } else {
                std::env::remove_var("MUR_HOME");
            }
        }
    }

    #[test]
    fn reserved_local_files_are_rejected() {
        // A package carrying private identity material must be refused before
        // extraction (which would plant/overwrite the agent's signing key).
        use std::collections::BTreeMap;
        for reserved in RESERVED_LOCAL_FILES {
            let mut files = BTreeMap::new();
            files.insert("profile.yaml".to_string(), b"ok".to_vec());
            files.insert((*reserved).to_string(), b"ATTACKER-KEY".to_vec());
            let archive = MuragentArchive { files };
            assert!(
                reject_reserved_local_files(&archive).is_err(),
                "must reject package carrying {reserved}"
            );
        }
        // A clean payload passes.
        let mut files = BTreeMap::new();
        files.insert("profile.yaml".to_string(), b"ok".to_vec());
        files.insert("skills/demo.md".to_string(), b"skill".to_vec());
        let archive = MuragentArchive { files };
        assert!(reject_reserved_local_files(&archive).is_ok());
    }

    #[test]
    fn revoked_author_key_is_refused() {
        use std::collections::BTreeMap;
        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path();
        let pk = [7u8; 32];
        let author = format!("ed25519:{}", B64.encode(pk));
        let list = trust::RevocationsList {
            version: 1,
            this_update: chrono::Utc::now(),
            next_update: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::days(30),
            crl_number: 1,
            revoked: vec![trust::RevokedEntry::Author {
                pubkey: author,
                reason: "compromised".into(),
                revoked_at: chrono::Utc::now(),
            }],
        };
        std::fs::create_dir_all(mur_home.join("trust")).unwrap();
        std::fs::write(
            mur_home.join("trust").join("revocations.json"),
            serde_json::to_vec(&list).unwrap(),
        )
        .unwrap();

        let archive = MuragentArchive {
            files: BTreeMap::new(),
        };
        assert!(matches!(
            check_revocations(&archive, mur_home, &pk, None),
            Err(MuragentError::TrustRefused(_))
        ));
        // A different (non-revoked) key passes, against a present, non-expired
        // list → Clean with the list's crl_number.
        assert!(matches!(
            check_revocations(&archive, mur_home, &[8u8; 32], None),
            Ok(trust::RevocationStatus::Clean { crl_number: 1 })
        ));
        // No cached list → Unknown (fail-open).
        let empty = TempDir::new().unwrap();
        assert!(matches!(
            check_revocations(&archive, empty.path(), &pk, None),
            Ok(trust::RevocationStatus::Unknown)
        ));
        // A rolled-back cache (crl_number below what we've accepted) is ignored,
        // not trusted — even though it lists the key, it yields Unknown.
        assert!(matches!(
            check_revocations(&archive, mur_home, &pk, Some(5)),
            Ok(trust::RevocationStatus::Unknown)
        ));
    }

    #[test]
    fn revoked_old_key_cannot_authorize_rotation() {
        // Revocation must beat rotation: a compromised+revoked old key cannot
        // sign itself a successor, or it could hijack the agent identity.
        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path();
        let old_pk_b64 = B64.encode([3u8; 32]);
        let list = trust::RevocationsList {
            version: 1,
            this_update: chrono::Utc::now(),
            next_update: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::days(30),
            crl_number: 1,
            revoked: vec![trust::RevokedEntry::Author {
                pubkey: format!("ed25519:{old_pk_b64}"),
                reason: "compromised".into(),
                revoked_at: chrono::Utc::now(),
            }],
        };
        std::fs::create_dir_all(mur_home.join("trust")).unwrap();
        std::fs::write(
            mur_home.join("trust").join("revocations.json"),
            serde_json::to_vec(&list).unwrap(),
        )
        .unwrap();

        let entry = |pk_b64: String| TrustEntry {
            public_key: pk_b64,
            display_name_seen: "Coach".into(),
            first_seen: "2026-01-01T00:00:00Z".into(),
            last_seen: "2026-01-01T00:00:00Z".into(),
            last_seen_surface: "cli".into(),
            trust_level: TrustLevel::Known,
            fingerprint: "ff".into(),
            word_list: "a b c d".into(),
            rotated_from: None,
            superseded_at: None,
            last_rotation_at: None,
        };
        let mut ts = TrustStore::default();

        // Revoked old key → refused with the revocation reason, before any
        // rotation manifest is even consulted.
        let revoked = entry(old_pk_b64);
        let err =
            try_apply_rotation(&mut ts, Some(&revoked), "NEWKEY", "Coach", mur_home).unwrap_err();
        assert!(err.contains("revoked"), "got: {err}");

        // A non-revoked old key falls through to the (absent) rotation manifest,
        // i.e. it is NOT short-circuited by the revocation check.
        let clean = entry(B64.encode([9u8; 32]));
        let err2 =
            try_apply_rotation(&mut ts, Some(&clean), "NEWKEY", "Coach", mur_home).unwrap_err();
        assert!(
            !err2.contains("revoked"),
            "should fail for a different reason, got: {err2}"
        );
    }

    #[test]
    fn display_name_slug_roundtrip() {
        assert_eq!(display_name_slug("My Agent"), "my-agent");
        assert_eq!(display_name_slug("Coach (Beta)"), "coach-beta");
        assert_eq!(display_name_slug("test"), "test");
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

    #[test]
    fn update_preserves_local_identity_keypair() {
        // Regression: loading a template-mode (.muragent carries no private
        // key) package over an existing agent must NOT delete the agent's
        // locally-minted identity keypair, or `mur agent export` afterward
        // fails with "identity files not found".
        let _guard = crate::trust::test_env_lock::MUR_HOME_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path().join("mur");
        let prev = std::env::var_os("MUR_HOME");
        unsafe { std::env::set_var("MUR_HOME", &mur_home) };

        let pkg = make_test_package(&tmp);
        let archive = MuragentArchive::read(&pkg).unwrap();
        let outcome = install(&archive, &mur_home, "test").unwrap();
        let slug = outcome.manifest.agent.slug.clone();
        let agent_dir = mur_home.join("agents").join(&slug);

        // Simulate a locally-minted keypair (as `mur agent create` writes).
        fs::write(agent_dir.join("identity.key"), b"PRIVATE-KEY").unwrap();
        fs::write(agent_dir.join("identity.pub"), b"PUBLIC-KEY").unwrap();

        // Re-install (same archive, same UUID) → update path runs clear_except_data.
        let outcome2 = install(&archive, &mur_home, "test").unwrap();
        assert!(outcome2.was_update);

        assert!(
            agent_dir.join("identity.key").exists(),
            "identity.key must survive an in-place update"
        );
        assert_eq!(
            fs::read(agent_dir.join("identity.key")).unwrap(),
            b"PRIVATE-KEY"
        );
        assert!(
            agent_dir.join("identity.pub").exists(),
            "identity.pub must survive an in-place update"
        );

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
