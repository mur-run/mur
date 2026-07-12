//! `mur model` subcommands — manage entries in `~/.mur/models.yaml`.

use std::path::Path;

use anyhow::Context;
use chrono::Utc;
use clap::{Args, Subcommand};
use mur_common::agent::AgentProfile;
use mur_common::model::{ModelEntry, ModelRegistry, RoleEntry};
use mur_common::route::{RouteDecision, RoutePolicy, RouteTier, TaskType};
use mur_common::secret::SecretRef;

use crate::model_prices::{self, PriceInfo};
use crate::route::Router;
use crate::route::ledger::EscalationLedger;

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
        /// Routing tier: local or frontier.
        #[arg(long)]
        tier: Option<String>,
        /// Estimated USD per 1000 input tokens.
        #[arg(long)]
        input_cost: Option<f64>,
        /// Estimated USD per 1000 output tokens.
        #[arg(long)]
        output_cost: Option<f64>,
        /// Estimated USD per 1000 output tokens (deprecated — use --output-cost).
        #[arg(long)]
        cost_per_1k: Option<f64>,
        /// Skip automatic price lookup from the models.dev catalog.
        #[arg(long)]
        no_fetch: bool,
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
    /// Test routing decisions (dry-run — no spawn).
    Route {
        #[command(subcommand)]
        sub: RouteSubCmd,
    },
    /// Manage the cached models.dev price catalog.
    Prices {
        #[command(subcommand)]
        sub: PricesSubCmd,
    },
    /// Set the global default model_ref (config.yaml `models.default`).
    /// Used when a profile has no `model_ref`/`model` set. The ref must
    /// already exist in `~/.mur/models.yaml` (fail-closed).
    Default {
        /// Registry key from `~/.mur/models.yaml` (e.g. claude_sonnet)
        model_ref: String,
    },
    /// Set the global fallback chain (config.yaml `models.fallback_chain`),
    /// tried in order after the primary model fails. Pass no refs to clear
    /// the chain. Each ref must already exist in `~/.mur/models.yaml`.
    Fallback {
        /// Ordered registry keys, tried after the primary fails. Omit to
        /// clear the chain.
        #[arg(num_args = 0..)]
        model_refs: Vec<String>,
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
        /// Routing policy: auto, prefer-local, force-local, or
        /// force-frontier:<model-id>.
        #[arg(long)]
        route_policy: Option<String>,
    },
    /// List configured roles.
    List,
}

#[derive(Subcommand, Debug)]
pub enum RouteSubCmd {
    /// Estimate difficulty and routing decision for a task (dry-run).
    Estimate {
        /// Task description.
        prompt: String,
        /// Task type for difficulty scoring.
        #[arg(long, default_value = "general")]
        task_type: String,
        /// Estimated context tokens needed.
        #[arg(long, default_value_t = 1000)]
        tokens: u64,
        /// Role for override lookup.
        #[arg(long)]
        role: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
        /// Persist this decision to the escalation ledger.
        #[arg(long)]
        record: bool,
    },
    /// Show escalation audit ledger statistics.
    Ledger {
        /// Number of days to scan.
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum PricesSubCmd {
    /// Delete the local cache so the next lookup re-fetches from models.dev.
    Refresh,
    /// Look up current pricing for a registered model by registry name.
    Show {
        /// Registry entry name (e.g. anthropic_opus_4_7).
        name: String,
    },
}

/// Populate cost fields on a partially-built [`ModelEntry`].
///
/// `--output-cost` takes precedence over the deprecated `--cost-per-1k`.
/// When only `--cost-per-1k` is given (and `--output-cost` is absent) it maps
/// to `output_cost_per_1k` for back-compat; `cost_per_1k_tokens` is also set
/// so the `effective_costs()` fallback chain resolves correctly.
pub fn build_entry_costs(
    mut entry: ModelEntry,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
    cost_per_1k: Option<f64>,
) -> ModelEntry {
    entry.input_cost_per_1k = input_cost;
    // --output-cost wins; fall back to --cost-per-1k for the output slot.
    entry.output_cost_per_1k = output_cost.or(cost_per_1k);
    // Retain the deprecated field so existing callers that read it directly
    // still get a value when only the old flag was used.
    if output_cost.is_none() {
        entry.cost_per_1k_tokens = cost_per_1k;
    }
    entry
}

/// Fill only fields that are still `None` on `entry` with values from `fetched`.
/// Explicit flag values set by `build_entry_costs` always take precedence.
/// If `fetched` is `None` the entry is returned unchanged.
pub fn apply_fetched_prices(mut entry: ModelEntry, fetched: Option<PriceInfo>) -> ModelEntry {
    if let Some(p) = fetched {
        if entry.input_cost_per_1k.is_none() {
            entry.input_cost_per_1k = Some(p.input_per_1k);
        }
        if entry.output_cost_per_1k.is_none() {
            entry.output_cost_per_1k = Some(p.output_per_1k);
        }
        if entry.context_window.is_none() {
            entry.context_window = p.context_window;
        }
    }
    entry
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
            tier,
            input_cost,
            output_cost,
            cost_per_1k,
            no_fetch,
        } => {
            let secret_ref = secret.map(|s| s.parse::<SecretRef>()).transpose()?;
            let tier = tier
                .as_deref()
                .map(|t| match t.to_lowercase().as_str() {
                    "local" => Ok(RouteTier::Local),
                    "frontier" => Ok(RouteTier::Frontier),
                    other => anyhow::bail!("invalid tier: {other}. Valid: local, frontier"),
                })
                .transpose()?;
            let entry = build_entry_costs(
                ModelEntry {
                    provider: provider.clone(),
                    model: model.clone(),
                    base_url,
                    secret: secret_ref,
                    capabilities,
                    params: serde_json::Value::Null,
                    tier,
                    ..Default::default()
                },
                input_cost,
                output_cost,
                cost_per_1k,
            );
            let is_local = matches!(tier, Some(RouteTier::Local));
            let entry = if no_fetch {
                entry
            } else {
                let mur_home = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let fetched = model_prices::lookup(&mur_home, &provider, &model, is_local);
                let e = apply_fetched_prices(entry, fetched);
                if let (Some(i), Some(o)) = (e.input_cost_per_1k, e.output_cost_per_1k) {
                    println!("Auto-filled pricing: input ${i}/1k, output ${o}/1k");
                }
                e
            };
            reg.models.insert(name.clone(), entry);
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
            let (inp, out) = e.effective_costs();
            if let Some(i) = inp {
                println!("  input $/1k:  {i}");
            }
            if let Some(o) = out {
                println!("  output $/1k: {o}");
            }
            if let Some(c) = e.context_window {
                println!("  context:     {c}");
            }
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
        ModelCmd::Route { sub } => cmd_route(&sub, &reg)?,
        ModelCmd::Prices { sub } => {
            let mur_home = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            cmd_prices(sub, &mur_home, &reg)?;
        }
        ModelCmd::Default { model_ref } => {
            cmd_model_default(&crate::cmd::agent::resolve_mur_home()?, &model_ref)?
        }
        ModelCmd::Fallback { model_refs } => {
            cmd_model_fallback(&crate::cmd::agent::resolve_mur_home()?, &model_refs)?
        }
    }
    Ok(())
}

fn cmd_prices(
    sub: PricesSubCmd,
    mur_home: &std::path::Path,
    reg: &ModelRegistry,
) -> anyhow::Result<()> {
    match sub {
        PricesSubCmd::Refresh => {
            let cache = model_prices::cache_path(mur_home);
            match std::fs::remove_file(&cache) {
                Ok(()) => println!("Price cache cleared: {}", cache.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    println!("Price cache not present (nothing to clear)");
                }
                Err(e) => return Err(e.into()),
            }
        }
        PricesSubCmd::Show { name } => {
            let entry = reg
                .models
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!("not found in registry: {name}"))?;
            let is_local = matches!(entry.tier, Some(RouteTier::Local));
            match model_prices::lookup(mur_home, &entry.provider, &entry.model, is_local) {
                Some(p) => {
                    println!("Pricing for {name} ({}/{}):", entry.provider, entry.model);
                    println!("  input:   ${}/1k tokens", p.input_per_1k);
                    println!("  output:  ${}/1k tokens", p.output_per_1k);
                    if let Some(ctx) = p.context_window {
                        println!("  context: {ctx} tokens");
                    }
                }
                None => {
                    println!(
                        "No pricing found for {name} ({}/{})",
                        entry.provider, entry.model
                    );
                }
            }
        }
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
            ..Default::default()
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
            route_policy,
        } => {
            let policy = route_policy
                .as_deref()
                .map(parse_route_policy)
                .transpose()?;
            reg.roles.insert(
                role.clone(),
                RoleEntry {
                    primary: model.clone(),
                    fallback,
                    cost_budget_per_day_usd: budget,
                    privacy_local_only,
                    route_policy: policy,
                },
            );
            reg.save_to(path)?;
            println!("Role {role} → {model}");
            if let Some(ref p) = route_policy {
                println!("  route policy: {p}");
            }
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

fn cmd_route(sub: &RouteSubCmd, reg: &ModelRegistry) -> anyhow::Result<()> {
    match sub {
        RouteSubCmd::Estimate {
            prompt,
            task_type,
            tokens,
            role,
            json,
            record,
        } => {
            let tt = parse_task_type(task_type)?;
            let router = Router::new(reg.clone())?;
            let timestamp = Utc::now().to_rfc3339();
            let event = router.audit(prompt, tt, *tokens, role.as_deref(), &timestamp);

            // Optionally persist so `ledger` can show the savings.
            if *record {
                let mut ledger = EscalationLedger::open_default()?;
                ledger.append(&event)?;
                ledger.flush()?;
            }

            if *json {
                let out = serde_json::json!({
                    "prompt": prompt,
                    "task_type": task_type,
                    "estimated_tokens": tokens,
                    "role": role,
                    "difficulty_score": event.difficulty_score,
                    "decision": event.decision,
                    "estimated_cost_usd": event.estimated_cost_usd,
                    "counterfactual_cost_usd": event.counterfactual_cost_usd,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("Task:             {prompt}");
                println!("Type:             {task_type}");
                println!("Tokens:           ~{tokens}");
                if let Some(r) = role {
                    println!("Role:             {r}");
                }
                println!("Score:            {:.3}", event.difficulty_score);
                println!("Decision:         {}", event.decision);
                println!("Est. cost:        ${:.4}", event.estimated_cost_usd);
                if event.counterfactual_cost_usd > 0.0 {
                    println!(
                        "Savings (if local): ${:.4}",
                        event.counterfactual_cost_usd - event.estimated_cost_usd
                    );
                }
            }
        }
        RouteSubCmd::Ledger { days, json } => {
            let ledger_dir = crate::paths::mur_root(None).join("route").join("ledger");
            let events = EscalationLedger::replay_days(&ledger_dir, *days);
            let s = EscalationLedger::summary(&ledger_dir, *days);

            if *json {
                let out = serde_json::json!({
                    "days_scanned": days,
                    "total_decisions": s.total,
                    "escalations": s.escalations,
                    "escalation_rate": s.rate,
                    "spend_usd": s.spend_usd,
                    "savings_usd": s.savings_usd,
                    "events": events,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("Escalation ledger ({days}d):");
                println!("  Total decisions: {}", s.total);
                println!("  Escalations:     {}", s.escalations);
                println!("  Escalation rate: {:.1}%", s.rate * 100.0);
                println!("  Spend:           ${:.4}", s.spend_usd);
                println!("  Savings:         ${:.4}", s.savings_usd);
                if s.total > 0 {
                    println!();
                    for event in &events {
                        let mark = match &event.decision {
                            RouteDecision::Local { .. } => "LOCAL",
                            RouteDecision::Escalate { .. } => "ESCALATE",
                        };
                        println!(
                            "  [{mark}] {:.3} | ${:.4} | {}",
                            event.difficulty_score, event.estimated_cost_usd, event.task_summary,
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_task_type(s: &str) -> anyhow::Result<TaskType> {
    match s.to_lowercase().as_str() {
        "codegen" | "code_gen" | "code" => Ok(TaskType::CodeGen),
        "codereview" | "code_review" | "review" => Ok(TaskType::CodeReview),
        "retrieval" | "search" | "retrieve" => Ok(TaskType::Retrieval),
        "refactor" => Ok(TaskType::Refactor),
        "documentation" | "docs" | "doc" => Ok(TaskType::Documentation),
        "debugging" | "debug" => Ok(TaskType::Debugging),
        "execution" | "exec" | "run" => Ok(TaskType::Execution),
        "general" | "chat" | "qa" => Ok(TaskType::General),
        other => anyhow::bail!(
            "unknown task type: {other}. Valid: codegen, codereview, retrieval, \
             refactor, documentation, debugging, execution, general"
        ),
    }
}

fn parse_route_policy(raw: &str) -> anyhow::Result<RoutePolicy> {
    match raw {
        "auto" => Ok(RoutePolicy::Auto),
        "prefer-local" | "prefer_local" => Ok(RoutePolicy::PreferLocal),
        "force-local" | "force_local" => Ok(RoutePolicy::ForceLocal),
        other if other.starts_with("force-frontier:") || other.starts_with("force_frontier:") => {
            let model_id = other
                .split_once(':')
                .map(|x| x.1)
                .ok_or_else(|| anyhow::anyhow!("force-frontier requires :<model-id>"))?
                .trim()
                .to_string();
            if model_id.is_empty() {
                anyhow::bail!("force-frontier requires a model ID after ':'");
            }
            Ok(RoutePolicy::ForceFrontier { model_id })
        }
        other => anyhow::bail!(
            "invalid route policy: {other}. Valid: auto, prefer-local, force-local, \
             force-frontier:<model-id>"
        ),
    }
}

/// Fail-closed guard: reject any model_ref that isn't already registered in
/// `<home>/models.yaml`. Used by both the global (`mur model default` /
/// `fallback`) and per-agent (`mur agent fallback`) setters so a typo'd ref
/// can never be silently persisted.
fn ensure_ref_exists(home: &Path, r: &str) -> anyhow::Result<()> {
    let reg = ModelRegistry::load_from(&home.join("models.yaml"))?;
    anyhow::ensure!(
        reg.models.contains_key(r),
        "model_ref {r:?} not in models.yaml"
    );
    Ok(())
}

/// Set the global default model_ref (`config.yaml` `models.default`).
pub fn cmd_model_default(home: &Path, model_ref: &str) -> anyhow::Result<()> {
    ensure_ref_exists(home, model_ref)?;
    let mut cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    cfg.models.default = Some(model_ref.to_string());
    crate::store::config::save_config_at(&home.join("config.yaml"), &cfg)?;
    println!("global default model = {model_ref}");
    Ok(())
}

/// Set the global fallback chain (`config.yaml` `models.fallback_chain`).
/// An empty slice clears the chain.
pub fn cmd_model_fallback(home: &Path, refs: &[String]) -> anyhow::Result<()> {
    for r in refs {
        ensure_ref_exists(home, r)?;
    }
    let mut cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    cfg.models.fallback_chain = refs.to_vec();
    crate::store::config::save_config_at(&home.join("config.yaml"), &cfg)?;
    println!("global fallback chain = {}", refs.join(", "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_default_validates_ref_exists() {
        use mur_common::model::{ModelEntry, ModelRegistry};
        // Temp home with a seeded models.yaml containing `claude_sonnet`.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "claude_sonnet".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-sonnet-5".into(),
                ..Default::default()
            },
        );
        reg.save_to(&home.join("models.yaml")).unwrap();

        // Unknown ref → error (fail-closed); known ref → persisted to config.yaml.
        assert!(cmd_model_default(&home, "does_not_exist").is_err());
        cmd_model_default(&home, "claude_sonnet").unwrap();
        let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
        assert_eq!(cfg.models.default.as_deref(), Some("claude_sonnet"));
    }

    #[test]
    fn set_fallback_validates_all_refs_and_clears_on_empty() {
        use mur_common::model::{ModelEntry, ModelRegistry};
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let mut reg = ModelRegistry::default();
        for key in ["claude_sonnet", "deepseek_v4_pro"] {
            reg.models.insert(
                key.into(),
                ModelEntry {
                    provider: "anthropic".into(),
                    model: key.into(),
                    ..Default::default()
                },
            );
        }
        reg.save_to(&home.join("models.yaml")).unwrap();

        // One unknown ref in the chain → whole call fails, nothing persisted.
        assert!(
            cmd_model_fallback(
                &home,
                &["claude_sonnet".to_string(), "does_not_exist".to_string()]
            )
            .is_err()
        );

        cmd_model_fallback(
            &home,
            &["claude_sonnet".to_string(), "deepseek_v4_pro".to_string()],
        )
        .unwrap();
        let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
        assert_eq!(
            cfg.models.fallback_chain,
            vec!["claude_sonnet", "deepseek_v4_pro"]
        );

        // Empty slice clears the chain.
        cmd_model_fallback(&home, &[]).unwrap();
        let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
        assert!(cfg.models.fallback_chain.is_empty());
    }
}
