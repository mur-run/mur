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
use crate::agent::McpNetMode;
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
    /// Names of MCP servers whose `network.mode` was downgraded from
    /// `BroadAudited` to `Inherit` (with `authorization` cleared) during this
    /// install. A bundle author's broad-egress grant is tied to a local
    /// operator (Task 3) and must not travel with the bundle; empty when
    /// nothing was downgraded.
    pub downgraded_broad_egress: Vec<String>,
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

    let downgraded_broad_egress = downgrade_profile_egress(&agent_dir)?;

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
        downgraded_broad_egress,
    })
}

/// For every MCP server whose `network.mode == BroadAudited`, resets the mode
/// to `Inherit` and clears `authorization`. A bundle author's broad-audited
/// grant is tied to their local operator consent (Task 3); it must not travel
/// with the bundle, so the importer must explicitly re-grant it. Returns the
/// names of the downgraded servers, in profile order. Pure / filesystem-free
/// so it's unit-testable directly.
fn downgrade_broad_egress(profile: &mut AgentProfile) -> Vec<String> {
    let mut downgraded = Vec::new();
    for server in &mut profile.mcp_servers {
        if let Some(network) = server.network.as_mut()
            && network.mode == McpNetMode::BroadAudited
        {
            network.mode = McpNetMode::Inherit;
            network.authorization = None;
            downgraded.push(server.name.clone());
        }
    }
    downgraded
}

/// Reads the just-extracted `profile.yaml`, downgrades any `BroadAudited` MCP
/// egress grants, and — only if anything changed — re-serializes it back to
/// disk. Returns the names of downgraded servers (empty if none).
fn downgrade_profile_egress(agent_dir: &Path) -> Result<Vec<String>, MuragentError> {
    let profile_path = agent_dir.join("profile.yaml");
    let raw = fs::read_to_string(&profile_path).map_err(MuragentError::Io)?;
    let mut profile: AgentProfile =
        serde_yaml_ng::from_str(&raw).map_err(|e| MuragentError::Other(e.to_string()))?;

    let downgraded = downgrade_broad_egress(&mut profile);
    if !downgraded.is_empty() {
        let out =
            serde_yaml_ng::to_string(&profile).map_err(|e| MuragentError::Other(e.to_string()))?;
        fs::write(&profile_path, out).map_err(MuragentError::Io)?;
    }
    Ok(downgraded)
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

    #[test]
    fn downgrade_broad_egress_resets_mode_and_clears_authorization() {
        use crate::agent::{EgressAuthorization, McpServerEntry, McpServerNetwork};

        let mut profile = AgentProfile::default_for_tests();
        profile.mcp_servers = vec![
            McpServerEntry {
                name: "broad-server".to_string(),
                command: "true".to_string(),
                network: Some(McpServerNetwork {
                    mode: McpNetMode::BroadAudited,
                    authorization: Some(EgressAuthorization {
                        authorized_by: "operator".to_string(),
                        authorized_at_ms: 1,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            McpServerEntry {
                name: "restricted-server".to_string(),
                command: "true".to_string(),
                network: Some(McpServerNetwork {
                    mode: McpNetMode::Restricted,
                    allow_hosts: vec!["example.com".to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ];

        let downgraded = downgrade_broad_egress(&mut profile);

        assert_eq!(downgraded, vec!["broad-server".to_string()]);

        let broad = &profile.mcp_servers[0].network.as_ref().unwrap();
        assert_eq!(broad.mode, McpNetMode::Inherit);
        assert_eq!(broad.authorization, None);

        let restricted = &profile.mcp_servers[1].network.as_ref().unwrap();
        assert_eq!(restricted.mode, McpNetMode::Restricted);
        assert_eq!(restricted.allow_hosts, vec!["example.com".to_string()]);
    }

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
