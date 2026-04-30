//! `mur model` subcommands — manage entries in `~/.mur/models.yaml`.

use anyhow::Context;
use clap::{Args, Subcommand};
use mur_common::agent::AgentProfile;
use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::secret::SecretRef;

#[derive(Args, Debug)]
pub struct ModelArgs {
    #[command(subcommand)]
    pub cmd: ModelCmd,
}

#[derive(Subcommand, Debug)]
pub enum ModelCmd {
    /// Add or replace a model entry.
    Add {
        /// Stable id (e.g. anthropic_opus_4_7).
        name: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        base_url: Option<String>,
        /// SecretRef syntax: `env:VAR`, `keychain:svc/acct`, `file:/p[.age]`,
        /// `cmd:./script`.
        #[arg(long)]
        secret: Option<String>,
        #[arg(long, value_delimiter = ',')]
        capabilities: Vec<String>,
    },
    List,
    Show {
        name: String,
    },
    Remove {
        name: String,
    },
    /// Lift each agent's inline `model:` block into a registry entry and
    /// rewrite the profile to use `model_ref:`. Idempotent.
    Migrate {
        /// Show what would change without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn run(args: ModelArgs) -> anyhow::Result<()> {
    let path = ModelRegistry::default_path()?;
    let mut reg = ModelRegistry::load_from(&path)
        .with_context(|| format!("load registry {}", path.display()))?;
    match args.cmd {
        ModelCmd::Add {
            name,
            provider,
            model,
            base_url,
            secret,
            capabilities,
        } => {
            let secret_ref = secret.map(|s| s.parse::<SecretRef>()).transpose()?;
            reg.models.insert(
                name.clone(),
                ModelEntry {
                    provider,
                    model,
                    base_url,
                    secret: secret_ref,
                    capabilities,
                    params: serde_json::Value::Null,
                },
            );
            reg.save_to(&path)?;
            println!("Added model {name} → {}", path.display());
        }
        ModelCmd::List => {
            if reg.models.is_empty() {
                println!("(no models registered)");
            }
            for (n, e) in &reg.models {
                println!("{n}\t{}\t{}", e.provider, e.model);
            }
        }
        ModelCmd::Show { name } => {
            let e = reg
                .models
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!("not found: {name}"))?;
            print!("{}", serde_yaml_ng::to_string(e)?);
        }
        ModelCmd::Remove { name } => {
            if reg.models.remove(&name).is_some() {
                reg.save_to(&path)?;
                println!("Removed {name}");
            } else {
                anyhow::bail!("not found: {name}");
            }
        }
        ModelCmd::Migrate { dry_run } => cmd_migrate(dry_run)?,
    }
    Ok(())
}

fn cmd_migrate(dry_run: bool) -> anyhow::Result<()> {
    let agents_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no HOME"))?
        .join(".mur/agents");
    let registry_path = ModelRegistry::default_path()?;
    let mut reg = ModelRegistry::load_from(&registry_path)?;
    let mut migrated_agents: Vec<String> = Vec::new();

    if !agents_dir.exists() {
        println!("(no ~/.mur/agents directory; nothing to migrate)");
        return Ok(());
    }

    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        let pyaml = entry.path().join("profile.yaml");
        if !pyaml.exists() {
            continue;
        }
        let body = std::fs::read_to_string(&pyaml)?;
        let mut profile: AgentProfile =
            serde_yaml_ng::from_str(&body).with_context(|| format!("parse {}", pyaml.display()))?;
        if profile.model_ref.is_some() {
            continue;
        }
        let id = synthesize_model_id(&profile.model.provider, &profile.model.name);
        reg.models.entry(id.clone()).or_insert_with(|| ModelEntry {
            provider: profile.model.provider.clone(),
            model: profile.model.name.clone(),
            base_url: None,
            secret: None,
            capabilities: vec![],
            params: serde_json::Value::Null,
        });
        profile.model_ref = Some(id.clone());
        migrated_agents.push(format!("{} → {id}", profile.name));
        if !dry_run {
            let new = serde_yaml_ng::to_string(&profile)?;
            let tmp = pyaml.with_extension("yaml.tmp");
            std::fs::write(&tmp, new)?;
            std::fs::rename(&tmp, &pyaml)?;
        }
    }
    if !dry_run {
        reg.save_to(&registry_path)?;
    }
    println!("{} agents would migrate:", migrated_agents.len());
    for line in migrated_agents {
        println!("  {line}");
    }
    if dry_run {
        println!("(dry run — pass without --dry-run to apply)");
    }
    Ok(())
}

fn synthesize_model_id(provider: &str, model: &str) -> String {
    let sanitized = model.replace(['-', ':', '.', '/'], "_");
    format!("{provider}_{sanitized}")
}
