//! `mur skill evolve <name>` — self-evolution loop.

use anyhow::{Context, Result};
use std::path::Path;

pub struct EvolveOptions {
    pub skill_name: String,
    pub dry_run: bool,
    pub max_iterations: usize,
}

pub async fn cmd_evolve(home: &Path, opts: EvolveOptions) -> Result<()> {
    let config_path = home.join("config.yaml");
    let cfg = mur_common::config::Config::load_or_default(&config_path);
    let backend = cfg.conversations.ask.effective_backend(&cfg.llm);
    let llm = crate::conversations::backend::factory::build_for_stage(&backend, "skill.evolve")
        .context("build LLM client")?;

    // Resolve agent name from env or config.
    let agent_name = std::env::var("MUR_AGENT_NAME").unwrap_or_else(|_| "default".into());

    let result = crate::evolve::skill_evolve::evolve_skill(
        home,
        &agent_name,
        &opts.skill_name,
        &*llm,
        opts.max_iterations,
        opts.dry_run,
    )
    .await?;

    if opts.dry_run {
        println!("Dry run complete — no changes written.");
    }

    // Print summary
    if !result.diagnoses.is_empty() || result.new_version != result.original_version {
        println!(
            "Evolved {}: {} → {} (gen {}, score {:.2})",
            opts.skill_name,
            result.original_version,
            result.new_version,
            result.new_generation,
            result.quality_score,
        );
    }

    Ok(())
}
