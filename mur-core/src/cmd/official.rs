//! `mur official` — browse + install from the official MUR catalog.
use anyhow::{Context, Result};
use std::io::Write;

use crate::official::client::fetch_catalog;
use crate::official::install::{install_item, installed_agent_name};
use crate::official::model_selection::{ModelSelectionPolicy, ModelSelectionWarning};


pub(crate) async fn cmd_official_list() -> Result<()> {
    let base = crate::auth::server_url();
    let items = fetch_catalog(&reqwest::Client::new(), &base).await?;
    if items.is_empty() {
        println!("No official items published yet.");
        return Ok(());
    }
    println!("{:<32} {:<6} {:<8} DESCRIPTION", "ID", "TIER", "VERSION");
    for i in &items {
        println!(
            "{:<32} {:<6} {:<8} {}",
            i.id, i.tier, i.version, i.description
        );
    }
    if crate::auth::load_tokens().is_none() {
        println!(
            "\nLog in with `mur auth login` to install (pro items need a MUR Pro subscription)."
        );
    }
    Ok(())
}

pub(crate) async fn cmd_official_install(id: &str) -> Result<()> {
    install_item(id).await?;
    println!("Installed official item {id}");
    if let Some(name) = installed_agent_name(id) {
        println!("Talk to it with `mur agent cli {name}`.");
    }
    Ok(())
}

/// Interactive model selection for official agent/fleet install.
/// Stages: (1) policy choice, (2) preview, (3) confirm.
pub(crate) async fn cmd_official_install_interactive(
    id: &str,
    model_ref_override: Option<String>,
) -> Result<()> {
    // Download + verify license, but defer package extraction.
    let mur_home = crate::paths::mur_root(None);
    let tokens = crate::auth::load_tokens()
        .context("not logged in — run `mur auth login` first")?;
    let user_id = tokens
        .user_id
        .clone()
        .context("stored login has no account id")?;

    let base = crate::auth::server_url();
    let client = reqwest::Client::new();
    let (bytes, license) = crate::official::client::download_item(
        &client,
        &base,
        &tokens.access_token,
        id,
    )
    .await?;

    use mur_common::official::{LicenseCheck, check_license};
    use mur_common::skill::publisher_trust::MUR_OFFICIAL_LICENSE_KEY_FP;
    match check_license(&license, id, &user_id, MUR_OFFICIAL_LICENSE_KEY_FP) {
        LicenseCheck::Ok => {}
        other => anyhow::bail!("server returned an invalid license ({other:?})"),
    }

    // Extract manifest to read model requirements.
    use mur_common::muragent::reader::MuragentArchive;
    let archive = MuragentArchive::read_from_bytes(&bytes)
        .context("parse .muragent bundle")?;
    let manifest_yaml = archive
        .get_str("manifest.yaml")
        .context("read manifest.yaml from archive")?;
    let manifest: mur_common::muragent::manifest::MuragentManifest =
        serde_yaml_ng::from_str(manifest_yaml).context("parse manifest.yaml")?;

    // Load model registry for planning.
    let registry = mur_common::model::ModelRegistry::load_from(
        &mur_home.join("models.yaml")
    )
    .context("load model registry — may need to run `mur models sync` first")?;

    // Stage 1: Policy choice (unless overridden).
    let selected_policy = if let Some(ref _model_ref) = model_ref_override {
        // If --model-ref provided, skip policy choice and use capability-first as default.
        crate::official::model_selection::ModelSelectionPolicy::CapabilityFirst
    } else {
        stage_1_choose_policy()?
    };

    // Stage 2: Plan model selection + preview.
    let requirements = manifest.model_requirements.as_ref().unwrap_or(&mur_common::muragent::manifest::ModelRequirements {
        chat: false,
        tools: false,
        minimum_context_window: None,
    });
    let plan = crate::official::model_selection::plan_model_selection(
        &registry,
        requirements,
        selected_policy,
        chrono::Utc::now(),
    )
    .context("plan model selection")?;

    stage_2_preview_and_confirm(id, &plan, &model_ref_override)?;

    // Persist license and install via existing path.
    crate::official::store::save_license(&mur_home, &license)?;
    let dir = tempfile::tempdir().context("temp dir")?;
    match id.split_once('/') {
        Some(("agents", name)) => {
            let p = dir.path().join(format!("{name}.muragent"));
            std::fs::write(&p, &bytes).context("write bundle")?;
            // Pass the primary model ref to override any wizard.
            let model_ref = model_ref_override.as_deref().or(Some(plan.primary.as_str()));
            crate::cmd::agent::install::cmd_install(&p, model_ref, None)?;
        }
        Some(("fleets", name)) => {
            let p = dir.path().join(format!("{name}.fleet"));
            std::fs::write(&p, &bytes).context("write bundle")?;
            crate::cmd::fleet::import::cmd_fleet_import(
                &mur_home,
                &p,
                crate::cmd::fleet::import::ImportOpts::default(),
            )?;
        }
        _ => anyhow::bail!("unknown catalog id '{id}'"),
    }

    println!("Installed official item {id}");
    if let Some(name) = installed_agent_name(id) {
        println!("Talk to it with `mur agent cli {name}`.");
    }
    Ok(())
}

/// Stage 1: Interactive policy choice.
fn stage_1_choose_policy() -> Result<ModelSelectionPolicy> {
    println!("\n┌─ Model Selection Policy ─────────────────────────┐");
    println!("│ How should we pick your models?                  │");
    println!("├──────────────────────────────────────────────────┤");
    println!("│ 1) Capability-first: Best model for the job      │");
    println!("│ 2) Cost-first: Fastest to run (cheapest)         │");
    println!("│ 3) Privacy-first: Local/self-hosted only         │");
    println!("└──────────────────────────────────────────────────┘");

    loop {
        print!("\nChoose (1-3): ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        match input.trim() {
            "1" => return Ok(ModelSelectionPolicy::CapabilityFirst),
            "2" => return Ok(ModelSelectionPolicy::CostFirst),
            "3" => return Ok(ModelSelectionPolicy::PrivacyFirst),
            _ => println!("Invalid choice. Enter 1, 2, or 3."),
        }
    }
}

/// Stage 2: Preview selected models and confirm install.
fn stage_2_preview_and_confirm(
    id: &str,
    plan: &crate::official::model_selection::ModelSelectionPlan,
    model_ref_override: &Option<String>,
) -> Result<()> {
    println!("\n┌─ Model Selection Preview ─────────────────────────┐");
    if let Some(override_ref) = model_ref_override {
        println!("│ Using specified model:                            │");
        println!("│   Primary: {:<40} │", override_ref);
    } else {
        println!("│ Recommended configuration:                        │");
        println!("│   Primary: {:<40} │", plan.primary);
        for (i, fallback) in plan.fallbacks.iter().enumerate() {
            println!("│   Fallback {}: {:<36} │", i + 1, fallback);
        }
    }

    if !plan.warnings.is_empty() {
        println!("├──────────────────────────────────────────────────┤");
        println!("│ ⚠ Warnings:                                       │");
        for warning in &plan.warnings {
            let msg = match warning {
                ModelSelectionWarning::UnverifiedToolCapability { model_ref } => {
                    format!("  {} unverified for tool use", model_ref)
                }
                ModelSelectionWarning::UnknownContextWindow { model_ref } => {
                    format!("  {} context size unknown", model_ref)
                }
                ModelSelectionWarning::UnknownPrice { model_ref } => {
                    format!("  {} price unknown", model_ref)
                }
                ModelSelectionWarning::StalePrice { model_ref } => {
                    format!("  {} price stale (>60d)", model_ref)
                }
            };
            println!("│ {:<46} │", msg);
        }
    }
    println!("└──────────────────────────────────────────────────┘");

    loop {
        print!("\nInstall {} with this configuration? (y/n): ", id);
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        match input.trim().to_lowercase().as_str() {
            "y" | "yes" => return Ok(()),
            "n" | "no" => anyhow::bail!("installation cancelled"),
            _ => println!("Please enter y or n."),
        }
    }
}
