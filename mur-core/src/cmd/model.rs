//! `mur model` subcommands — manage entries in `~/.mur/models.yaml`.

use anyhow::Context;
use clap::{Args, Subcommand};
use mur_common::agent::AgentProfile;
use mur_common::model::{ModelEntry, ModelRegistry, RoleEntry};
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
    /// Manage model roles (reflector, curator, embedding).
    Role {
        #[command(subcommand)]
        sub: RoleSubCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum RoleSubCmd {
    /// Assign a model to a role.
    Set {
        /// Role name (e.g. reflector, curator, embedding).
        role: String,
        /// Primary model ID from the registry.
        model: String,
        /// Fallback model ID.
        #[arg(long)]
        fallback: Option<String>,
        /// Daily cost budget in USD.
        #[arg(long)]
        budget: Option<f64>,
        /// Only use local models for sensitive data.
        #[arg(long)]
        privacy_local_only: bool,
    },
    /// List configured roles.
    List,
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
                    tier: None,
                    cost_per_1k_tokens: None,
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
        ModelCmd::Role { sub } => cmd_role(sub, &mut reg, &path)?,
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
            tier: None,
            cost_per_1k_tokens: None,
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

fn cmd_role(
    sub: RoleSubCmd,
    reg: &mut ModelRegistry,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    match sub {
        RoleSubCmd::Set {
            role,
            model,
            fallback,
            budget,
            privacy_local_only,
        } => {
            reg.roles.insert(
                role.clone(),
                RoleEntry {
                    primary: model.clone(),
                    fallback,
                    cost_budget_per_day_usd: budget,
                    privacy_local_only,
                    route_policy: None,
                },
            );
            reg.save_to(path)?;
            println!("Role {role} → {model}");
        }
        RoleSubCmd::List => {
            if reg.roles.is_empty() {
                println!("(no roles configured)");
                return Ok(());
            }
            println!("{:<15} {:<25} {:<25}", "ROLE", "PRIMARY", "FALLBACK");
            for (name, entry) in &reg.roles {
                println!(
                    "{:<15} {:<25} {:<25}",
                    name,
                    entry.primary,
                    entry.fallback.as_deref().unwrap_or("-"),
                );
            }
        }
    }
    Ok(())
}
