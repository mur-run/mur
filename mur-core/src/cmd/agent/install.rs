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

    maybe_resolve_model(&mur_home, &outcome.manifest.agent.slug, &archive)?;
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

fn maybe_resolve_model(mur_home: &Path, slug: &str, archive: &MuragentArchive) -> Result<()> {
    use crate::cmd::agent::model_resolve::{apply_model_choice, detect_hardware};
    use mur_common::model_resolve::{Recommendation, recommend};

    let manifest_yaml = archive
        .get_str("manifest.yaml")
        .context("read manifest.yaml")?;
    let manifest: MuragentManifest = serde_yaml_ng::from_str(manifest_yaml)?;
    let hint = manifest.model_hint.clone();
    let hw = detect_hardware();
    let rec = recommend(hint.as_ref(), &hw);

    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!(
            "  model: {} — set one with `mur model add` then run the agent with --model <ref>",
            match rec {
                Recommendation::Local => "local recommended (Ollama/MLX)",
                Recommendation::Cloud => "cloud model recommended",
                Recommendation::CloudOrSmallerLocal => "cloud or a smaller local model",
                Recommendation::NeutralMenu => "choose a backend",
            }
        );
        return Ok(());
    }

    println!("\nThis agent needs a model backend (no weights are bundled).");
    if let Some(h) = &hint {
        println!(
            "  Authored for: {}/{} (tier {:?})",
            h.provider, h.name, h.tier
        );
    }
    let choice = prompt_model_choice(&rec, hint.as_ref())?;
    if let Some(choice) = choice {
        let key = apply_model_choice(mur_home, slug, &choice)?;
        println!("  bound model_ref = {key}");
    } else {
        println!("  skipped — set one later with `mur model add`");
    }
    Ok(())
}

fn prompt_model_choice(
    rec: &mur_common::model_resolve::Recommendation,
    hint: Option<&mur_common::muragent::manifest::ModelHint>,
) -> Result<Option<crate::cmd::agent::model_resolve::ModelChoice>> {
    use crate::cmd::agent::model_resolve::ModelChoice;
    use mur_common::model_resolve::Recommendation;
    use std::io::Write;

    let default_local = hint.map(|h| (h.provider.clone(), h.name.clone()));
    let prompt = match rec {
        Recommendation::Local => "Pull a local model now? [Y/n] ",
        Recommendation::Cloud | Recommendation::CloudOrSmallerLocal => {
            "Paste an API key for a cloud model? [y/N] "
        }
        Recommendation::NeutralMenu => "Configure a model now? [y/N] ",
    };
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let yes = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes" | "");

    if matches!(rec, Recommendation::Local)
        && yes
        && let Some((provider, model)) = default_local
    {
        return Ok(Some(ModelChoice {
            provider,
            model,
            base_url: None,
            secret: None,
        }));
    }
    if !yes {
        return Ok(None);
    }
    let provider = read_field("  provider (e.g. anthropic): ")?;
    let model = read_field("  model (e.g. claude-opus-4-7): ")?;
    let secret = read_field("  secret ref (e.g. env:ANTHROPIC_API_KEY): ")?;
    Ok(Some(ModelChoice {
        provider,
        model,
        base_url: None,
        secret: if secret.is_empty() {
            None
        } else {
            Some(secret)
        },
    }))
}

fn read_field(prompt: &str) -> Result<String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}
