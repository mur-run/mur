//! `mur agent install <path>` / `mur agent uninstall <name>` / `mur agent inspect <path>`
//!
//! Thin CLI wrappers around the `mur_common::muragent::installer` flow. The
//! actual install logic — validation, trust upsert, payload extraction — lives
//! in mur-common and is shared with Hub and (future) Commander.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::muragent::installer::{self, InstallOutcome};
use mur_common::muragent::manifest::MuragentManifest;
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::validator;
use mur_common::trust;

use super::resolve_mur_home;

pub fn cmd_install(path: &Path) -> Result<()> {
    let archive = MuragentArchive::read(path)
        .with_context(|| format!("read .muragent file at {}", path.display()))?;
    let mur_home = resolve_mur_home()?;
    let outcome: InstallOutcome =
        installer::install(&archive, &mur_home, "cli").context("install .muragent")?;

    let verb = if outcome.was_update {
        "Updated"
    } else {
        "Installed"
    };
    println!(
        "{verb} agent '{}' ({})",
        outcome.manifest.agent.display_name, outcome.manifest.agent.slug
    );
    println!("  trust:       {:?}", outcome.trust_level);
    println!("  fingerprint: {}", outcome.fingerprint_hex);
    println!("  words:       {}", outcome.fingerprint_words);
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
