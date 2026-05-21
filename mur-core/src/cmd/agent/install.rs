//! `mur agent install <path>` / `mur agent uninstall <name>` / `mur agent inspect <path>`
//!
//! These commands manage `.muragent` v2 packages on the local host. Install
//! runs the full 11-step validation pipeline from `mur_common::muragent`,
//! checks the trust store for key-change-without-rotation attacks, and
//! extracts the payload into `~/.mur/agents/<slug>/`. Inspect prints
//! manifest details without modifying the local filesystem.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use mur_common::AgentProfile;
use mur_common::muragent::manifest::MuragentManifest;
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::validator::{self, ValidationResult};
use mur_common::trust::{self, TrustEntry, TrustLevel, TrustStore};

use super::resolve_mur_home;

/// Files in the .muragent that belong to the package envelope (not payload).
const ENVELOPE_FILES: &[&str] = &["manifest.yaml", "manifest.signed.json", "signatures.json"];

pub fn cmd_install(path: &Path) -> Result<()> {
    let archive = MuragentArchive::read(path)
        .with_context(|| format!("read .muragent file at {}", path.display()))?;

    // Full validation pipeline — fatal on any failure (spec §7.5)
    let result = validator::validate(&archive).context("validate .muragent")?;
    let manifest = &result.manifest;
    let slug = &manifest.agent.slug;
    let display_name = &manifest.agent.display_name;

    // Validate slug shape so we don't write to `agents/../../etc`
    mur_common::validate_agent_name(slug)
        .with_context(|| format!("invalid agent slug '{slug}' in manifest"))?;

    let mut trust = TrustStore::load().context("load trust store")?;
    let author_pubkey_b64 = B64.encode(result.author_pubkey);
    let existing_by_pubkey = trust.find_by_pubkey(&author_pubkey_b64).cloned();

    if existing_by_pubkey.is_none() {
        let by_name = trust.find_by_display_name(display_name);
        if !by_name.is_empty() {
            bail!(
                "Agent '{}' has a new signing key but no rotation manifest is present. \
                 This could indicate an impersonation attempt. \
                 Remove the existing trust entry first if this is intentional.",
                display_name
            );
        }
    }

    let mur_home = resolve_mur_home()?;
    let agent_dir = mur_home.join("agents").join(slug);

    if agent_dir.exists() {
        // Same UUID → update flow; different UUID → collision
        let existing_profile = agent_dir.join("profile.yaml");
        if existing_profile.exists() {
            let existing_yaml = fs::read_to_string(&existing_profile)
                .with_context(|| format!("read {}", existing_profile.display()))?;
            if let Ok(existing) = serde_yaml_ng::from_str::<AgentProfile>(&existing_yaml)
                && existing.id == manifest.agent.original_uuid
            {
                update_agent(&archive, &agent_dir, manifest)?;
                upsert_trust(&mut trust, &result, &author_pubkey_b64, &existing_by_pubkey)?;
                trust.save().context("save trust store")?;
                return Ok(());
            }
        }
        bail!(
            "agent '{slug}' already exists at {}. Run 'mur agent remove {slug}' first, \
             or rename the existing agent.",
            agent_dir.display()
        );
    }

    fs::create_dir_all(&agent_dir).context("create agent directory")?;
    extract_payload(&archive, &agent_dir)?;
    upsert_trust(&mut trust, &result, &author_pubkey_b64, &existing_by_pubkey)?;
    trust.save().context("save trust store")?;

    println!("Installed agent '{display_name}' ({slug})");
    println!("  trust:       {:?}", trust_level_for(&existing_by_pubkey));
    println!(
        "  fingerprint: {}",
        trust::short_fingerprint(&result.author_pubkey)
    );
    Ok(())
}

pub fn cmd_uninstall(name: &str, delete_data: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_dir = mur_home.join("agents").join(name);

    if !agent_dir.exists() {
        bail!("agent '{name}' is not installed");
    }

    if delete_data {
        fs::remove_dir_all(&agent_dir).context("remove agent directory")?;
        println!("Uninstalled '{name}' and deleted all data");
    } else {
        for entry in fs::read_dir(&agent_dir)? {
            let entry = entry?;
            if entry.file_name() == "data" {
                continue;
            }
            let p = entry.path();
            if p.is_dir() {
                fs::remove_dir_all(&p)?;
            } else {
                fs::remove_file(&p)?;
            }
        }
        println!(
            "Uninstalled '{name}' (data preserved at {}/data)",
            agent_dir.display()
        );
    }
    Ok(())
}

pub fn cmd_inspect(path: &Path) -> Result<()> {
    let archive = MuragentArchive::read(path)
        .with_context(|| format!("read .muragent file at {}", path.display()))?;

    let manifest_yaml = archive
        .get_str("manifest.yaml")
        .context("read manifest.yaml from archive")?;
    let manifest: MuragentManifest =
        serde_yaml_ng::from_str(manifest_yaml).context("parse manifest.yaml from archive")?;

    println!(
        "Agent:       {} ({})",
        manifest.agent.display_name, manifest.agent.slug
    );
    println!("Schema:      {}", manifest.schema);
    println!("Exported at: {}", manifest.exported_at);
    println!("Mur version: {}", manifest.exporter.mur_version);
    println!("Bundle ID:   {}", manifest.agent.bundle_id);
    println!("URL scheme:  {}", manifest.agent.url_scheme);
    println!("Agent UUID:  {}", manifest.agent.original_uuid);
    println!("Surfaces:    {:?}", manifest.required_surfaces);
    println!("Capabilities: {:?}", manifest.optional_capabilities);
    println!("MCP servers: {}", manifest.mcp_servers.len());
    for mcp in &manifest.mcp_servers {
        println!("  - {} ({})", mcp.name, mcp.command_basename);
    }

    match validator::validate(&archive) {
        Ok(result) => {
            println!();
            println!("Signature:    VALID");
            println!("Author keyid: {}", result.keyid);
            println!(
                "Fingerprint:  {}",
                trust::short_fingerprint(&result.author_pubkey)
            );
            println!(
                "Words:        {}",
                trust::word_list_fingerprint(&result.author_pubkey)
            );
        }
        Err(e) => {
            println!();
            println!("Signature:    INVALID — {e}");
        }
    }
    Ok(())
}

fn trust_level_for(existing: &Option<TrustEntry>) -> TrustLevel {
    existing
        .as_ref()
        .map(|e| e.trust_level.clone())
        .unwrap_or(TrustLevel::Pending)
}

fn upsert_trust(
    trust: &mut TrustStore,
    result: &ValidationResult,
    author_pubkey_b64: &str,
    existing: &Option<TrustEntry>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let first_seen = existing
        .as_ref()
        .map(|e| e.first_seen.clone())
        .unwrap_or_else(|| now.clone());
    let level = existing
        .as_ref()
        .map(|e| e.trust_level.clone())
        .unwrap_or(TrustLevel::Pending);

    trust.upsert(TrustEntry {
        public_key: author_pubkey_b64.to_string(),
        display_name_seen: result.manifest.agent.display_name.clone(),
        first_seen,
        last_seen: now,
        last_seen_surface: "cli".into(),
        trust_level: level,
        fingerprint: trust::short_fingerprint(&result.author_pubkey),
        word_list: trust::word_list_fingerprint(&result.author_pubkey),
        rotated_from: existing.as_ref().and_then(|e| e.rotated_from.clone()),
        superseded_at: existing.as_ref().and_then(|e| e.superseded_at.clone()),
        last_rotation_at: existing.as_ref().and_then(|e| e.last_rotation_at.clone()),
    });
    Ok(())
}

fn extract_payload(archive: &MuragentArchive, agent_dir: &Path) -> Result<()> {
    for (path, data) in &archive.files {
        if ENVELOPE_FILES.contains(&path.as_str()) {
            continue;
        }
        let dest = agent_dir.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        fs::write(&dest, data).with_context(|| format!("write payload file {}", dest.display()))?;
    }
    Ok(())
}

fn update_agent(
    archive: &MuragentArchive,
    agent_dir: &Path,
    manifest: &MuragentManifest,
) -> Result<()> {
    // Preserve any user data in data/, replace everything else
    for entry in fs::read_dir(agent_dir)? {
        let entry = entry?;
        if entry.file_name() == "data" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }

    extract_payload(archive, agent_dir)?;

    println!(
        "Updated '{}' to mur version {}",
        manifest.agent.display_name, manifest.exporter.mur_version
    );
    Ok(())
}
