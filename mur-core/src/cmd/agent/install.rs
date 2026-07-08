//! `mur agent install <path>` / `mur agent uninstall <name>` / `mur agent inspect <path>`
//!
//! Thin CLI wrappers around the `mur_common::muragent::installer` flow. The
//! actual install logic — validation, trust upsert, payload extraction — lives
//! in mur-common and is shared with Hub and (future) Commander.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::agent::AgentProfile;
use mur_common::identity::AgentIdentity;
use mur_common::muragent::installer::{self, InstallOutcome};
use mur_common::muragent::manifest::MuragentManifest;
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::validator;
use mur_common::trust;

use super::resolve_mur_home;

pub fn cmd_install(
    path: &Path,
    model_ref_override: Option<&str>,
    as_name: Option<&str>,
) -> Result<()> {
    let archive = MuragentArchive::read(path)
        .with_context(|| format!("read .muragent file at {}", path.display()))?;
    let mur_home = resolve_mur_home()?;
    let outcome: InstallOutcome = installer::install_with_name(&archive, &mur_home, "cli", as_name)
        .context("install .muragent")?;

    // The install/dispatch name: the clone's name when `--as` was given,
    // else the manifest's own slug (unchanged from prior behavior).
    let installed_name: &str = as_name.unwrap_or(&outcome.manifest.agent.slug);

    if let Some(clone_name) = as_name {
        clone_identity_and_profile(&mur_home, clone_name)?;
        println!("Cloned agent as '{clone_name}'");
    } else {
        let verb = if outcome.was_update {
            "Updated"
        } else {
            "Installed"
        };
        println!(
            "{verb} agent '{}' ({})",
            outcome.manifest.agent.display_name, outcome.manifest.agent.slug
        );
    }
    println!("  trust:       {:?}", outcome.trust_level);
    println!("  fingerprint: {}", outcome.fingerprint_hex);
    println!("  words:       {}", outcome.fingerprint_words);

    if !outcome.downgraded_broad_egress.is_empty() {
        let names = outcome.downgraded_broad_egress.join(", ");
        println!(
            "⚠ broad-audited egress was reset to inherit for: {names}. \
             Re-grant locally with: mur agent mcp set-network {installed_name} <server> --broad-audited"
        );
    }

    if let Some(model_ref) = model_ref_override {
        apply_model_ref_override(&mur_home, installed_name, model_ref)?;
    } else {
        maybe_resolve_model(&mur_home, installed_name, &archive)?;
    }
    Ok(())
}

/// After a `--as <name>` clone install, give the clone a new identity: a
/// fresh UUIDv7 `profile.id`, `profile.name` set to the clone's directory
/// name, and a freshly minted + persisted Ed25519 identity (never the
/// source agent's private key — the installer already strips
/// `identity.key` from any incoming payload).
fn clone_identity_and_profile(mur_home: &Path, clone_name: &str) -> Result<()> {
    let agent_dir = mur_home.join("agents").join(clone_name);
    let profile_path = agent_dir.join("profile.yaml");
    let mut profile: AgentProfile = serde_yaml_ng::from_str(
        &std::fs::read_to_string(&profile_path)
            .with_context(|| format!("read {}", profile_path.display()))?,
    )
    .with_context(|| format!("parse {}", profile_path.display()))?;

    profile.name = clone_name.to_string();
    profile.id = uuid::Uuid::now_v7().to_string();

    // Atomic write (temp + rename) so a crash mid-write never leaves a
    // half-written profile.yaml for the clone; matches the repo YAML convention.
    let tmp = profile_path.with_extension("yaml.tmp");
    std::fs::write(&tmp, serde_yaml_ng::to_string(&profile)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &profile_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), profile_path.display()))?;

    AgentIdentity::generate()
        .save(&agent_dir)
        .map_err(|e| anyhow::anyhow!("save fresh identity for clone '{clone_name}': {e}"))?;

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
    use mur_common::model_resolve::recommend;

    let manifest_yaml = archive
        .get_str("manifest.yaml")
        .context("read manifest.yaml")?;
    let manifest: MuragentManifest = serde_yaml_ng::from_str(manifest_yaml)?;
    let hint = manifest.model_hint.clone();
    let hw = detect_hardware();
    let rec = recommend(hint.as_ref(), &hw);

    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!(
            "  model: not configured — use `mur agent install --model <ref>` \
             or run `mur model add` then `mur agent start {slug}`"
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

/// Apply a `--model <ref>` override: verify the ref exists in the registry
/// and point the agent at it. Used by `--model` flag (non-interactive path).
fn apply_model_ref_override(mur_home: &Path, slug: &str, model_ref: &str) -> Result<()> {
    use crate::cmd::agent::model_resolve::ModelChoice;
    use mur_common::model::ModelRegistry;

    let reg_path = ModelRegistry::default_path()?;
    let reg = ModelRegistry::load_from(&reg_path)?;
    anyhow::ensure!(
        reg.models.contains_key(model_ref),
        "model ref '{model_ref}' not found in ~/.mur/models.yaml — \
         run `mur model add` to register it first"
    );
    let entry = &reg.models[model_ref];
    let choice = ModelChoice {
        provider: entry.provider.clone(),
        model: entry.model.clone(),
        base_url: entry.base_url.clone(),
        secret: entry.secret.as_ref().map(|s| s.to_string()),
    };
    let key = crate::cmd::agent::model_resolve::apply_model_choice(mur_home, slug, &choice)?;
    println!("  model_ref = {key}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_resolve_with_model_ref_applies_without_prompt() {
        use mur_common::model::{ModelEntry, ModelRegistry};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path();

        // Create agent dir with a default profile
        let agent_home = mur_home.join("agents").join("demo");
        std::fs::create_dir_all(&agent_home).unwrap();
        let p = mur_common::agent::AgentProfile::default_for_tests();
        std::fs::write(
            agent_home.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();

        // Pre-populate the registry so the ref resolves
        unsafe {
            std::env::set_var("MUR_HOME", mur_home);
        }
        let reg_path = ModelRegistry::default_path().unwrap();
        let mut reg = ModelRegistry::load_from(&reg_path).unwrap_or_default();
        reg.models.insert(
            "ollama_llama3_2_3b".to_string(),
            ModelEntry {
                provider: "ollama".into(),
                model: "llama3.2:3b".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
                ..Default::default()
            },
        );
        reg.save_to(&reg_path).unwrap();

        // Call the new apply_model_ref_override helper
        apply_model_ref_override(mur_home, "demo", "ollama_llama3_2_3b").unwrap();

        let profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(
            &std::fs::read_to_string(agent_home.join("profile.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(profile.model_ref.as_deref(), Some("ollama_llama3_2_3b"));
    }

    #[test]
    fn cmd_install_as_clones_with_new_id_and_fresh_identity() {
        use mur_common::agent::AgentProfile as Profile;
        use mur_common::identity::AgentIdentity as Identity;
        use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path().join("mur");
        unsafe {
            std::env::set_var("MUR_HOME", &mur_home);
        }

        // Build a source .muragent bundle with a known source profile id.
        let mut source_profile = Profile::default_for_tests();
        source_profile.name = "aura".to_string();
        let source_id = source_profile.id.clone();
        let manifest = build_manifest_from_profile(&source_profile, "2.13.0");
        let profile_yaml = serde_yaml_ng::to_string(&source_profile).unwrap();
        let writer = MuragentWriter::new(manifest, profile_yaml, Identity::generate());
        let bundle_path = tmp.path().join("aura.muragent");
        writer.write(&bundle_path).unwrap();

        cmd_install(&bundle_path, None, Some("clone-x")).unwrap();

        let clone_dir = mur_home.join("agents").join("clone-x");
        let profile: Profile = serde_yaml_ng::from_str(
            &std::fs::read_to_string(clone_dir.join("profile.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(profile.name, "clone-x");
        assert_ne!(profile.id, source_id, "clone must get a new profile.id");
        assert!(
            clone_dir.join("identity.key").exists(),
            "clone must have a freshly minted identity.key"
        );
        assert!(clone_dir.join("identity.pub").exists());
    }
}
