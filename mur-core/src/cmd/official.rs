//! `mur official` — browse + install from the official MUR catalog.
use std::io::{IsTerminal, stdin, stdout};

use anyhow::{Result, bail};

use crate::official::{
    client::fetch_catalog,
    install::{
        InstallOutcome, ModelSelection, OfficialInstallOptions, install_item, installed_agent_name,
    },
    model_selection::{ModelSelectionPolicy, ModelSelectionWarning},
};

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
pub enum OfficialModelPolicy {
    CapabilityFirst,
    CostFirst,
    PrivacyFirst,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallDecision {
    Prompt,
    Select(ModelSelection),
}

fn decide_install(
    id: &str,
    policy: Option<OfficialModelPolicy>,
    model_ref: Option<String>,
    fallbacks: Vec<String>,
    is_tty: bool,
) -> Result<InstallDecision> {
    if id.starts_with("fleets/") && (policy.is_some() || model_ref.is_some()) {
        bail!("model selection flags apply only to Agents with signed model requirements");
    }
    if !fallbacks.is_empty() && model_ref.is_none() {
        bail!("--fallback requires --model-ref");
    }
    if let Some(primary) = model_ref {
        return Ok(InstallDecision::Select(ModelSelection::Explicit {
            primary,
            fallbacks,
        }));
    }
    if let Some(policy) = policy {
        return Ok(InstallDecision::Select(ModelSelection::Automatic(
            policy.into(),
        )));
    }
    if id.starts_with("fleets/") {
        return Ok(InstallDecision::Select(ModelSelection::Unchanged));
    }
    if is_tty {
        Ok(InstallDecision::Prompt)
    } else {
        Ok(InstallDecision::Select(ModelSelection::Automatic(
            ModelSelectionPolicy::CapabilityFirst,
        )))
    }
}

impl From<OfficialModelPolicy> for ModelSelectionPolicy {
    fn from(value: OfficialModelPolicy) -> Self {
        match value {
            OfficialModelPolicy::CapabilityFirst => Self::CapabilityFirst,
            OfficialModelPolicy::CostFirst => Self::CostFirst,
            OfficialModelPolicy::PrivacyFirst => Self::PrivacyFirst,
        }
    }
}

pub(crate) async fn cmd_official_install(
    id: &str,
    policy: Option<OfficialModelPolicy>,
    model_ref: Option<String>,
    fallbacks: Vec<String>,
) -> Result<()> {
    let is_tty = stdin().is_terminal() && stdout().is_terminal();
    let base = crate::auth::server_url();
    let catalog = fetch_catalog(&reqwest::Client::new(), &base).await?;
    let item = catalog
        .iter()
        .find(|item| item.id == id)
        .ok_or_else(|| anyhow::anyhow!("official catalog item '{id}' not found"))?;
    ensure_client_compatible(item.min_mur_version.as_deref())?;
    if item.model_requirements.is_none() && (policy.is_some() || model_ref.is_some()) {
        bail!("model selection flags require an Agent with signed model requirements");
    }
    let decision = decide_install(id, policy, model_ref, fallbacks, is_tty)?;
    let selection = match decision {
        InstallDecision::Prompt => {
            let choices = ["Capability first", "Cost first", "Privacy first"];
            let selected = dialoguer::Select::new()
                .with_prompt("Model selection policy")
                .items(&choices)
                .default(0)
                .interact()?;
            ModelSelection::Automatic(match selected {
                0 => ModelSelectionPolicy::CapabilityFirst,
                1 => ModelSelectionPolicy::CostFirst,
                _ => ModelSelectionPolicy::PrivacyFirst,
            })
        }
        InstallDecision::Select(selection) => {
            if !is_tty
                && selection == ModelSelection::Automatic(ModelSelectionPolicy::CapabilityFirst)
            {
                println!("Non-interactive install: using capability-first model selection.");
            }
            selection
        }
    };

    // Explicit refs already express a reviewed chain. Policy installs are
    // confirmed in interactive terminals before any download/import mutation.
    if is_tty
        && !matches!(
            selection,
            ModelSelection::Explicit { .. } | ModelSelection::Unchanged
        )
    {
        let confirmed = dialoguer::Confirm::new()
            .with_prompt("Confirm installation?")
            .default(true)
            .interact()?;
        if !confirmed {
            bail!("installation cancelled");
        }
    }

    let outcome = install_item(
        id,
        &OfficialInstallOptions {
            model_selection: selection,
        },
    )
    .await?;
    print_outcome(&outcome);
    Ok(())
}

fn ensure_client_compatible(minimum: Option<&str>) -> Result<()> {
    crate::official::client::ensure_client_compatible(minimum)
}

fn print_outcome(outcome: &InstallOutcome) {
    if let Some(plan) = &outcome.model_selection {
        println!("Model selection:");
        println!("  primary: {}", plan.primary);
        for (index, fallback) in plan.fallbacks.iter().enumerate() {
            println!("  fallback {}: {fallback}", index + 1);
        }
        for warning in &plan.warnings {
            let message = match warning {
                ModelSelectionWarning::UnverifiedToolCapability { model_ref } => {
                    format!("{model_ref}: tool capability is unverified")
                }
                ModelSelectionWarning::UnknownContextWindow { model_ref } => {
                    format!("{model_ref}: context window is unknown")
                }
                ModelSelectionWarning::UnknownPrice { model_ref } => {
                    format!("{model_ref}: price is unknown")
                }
                ModelSelectionWarning::StalePrice { model_ref } => {
                    format!("{model_ref}: price is older than 60 days")
                }
            };
            println!("  warning: {message}");
        }
    }
    println!("Installed official item {}", outcome.item_id);
    if let Some(name) = installed_agent_name(&outcome.item_id) {
        println!("Talk to it with `mur agent cli {name}`.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_decision_covers_prompt_default_and_explicit_paths() {
        assert_eq!(
            decide_install("agents/x", None, None, vec![], true).unwrap(),
            InstallDecision::Prompt
        );
        assert_eq!(
            decide_install("agents/x", None, None, vec![], false).unwrap(),
            InstallDecision::Select(ModelSelection::Automatic(
                ModelSelectionPolicy::CapabilityFirst
            ))
        );
        assert_eq!(
            decide_install(
                "agents/x",
                Some(OfficialModelPolicy::PrivacyFirst),
                None,
                vec![],
                true,
            )
            .unwrap(),
            InstallDecision::Select(ModelSelection::Automatic(
                ModelSelectionPolicy::PrivacyFirst
            ))
        );
        assert_eq!(
            decide_install(
                "agents/x",
                None,
                Some("primary".into()),
                vec!["fallback".into()],
                true,
            )
            .unwrap(),
            InstallDecision::Select(ModelSelection::Explicit {
                primary: "primary".into(),
                fallbacks: vec!["fallback".into()],
            })
        );
    }

    #[test]
    fn minimum_version_gate_is_actionable() {
        assert!(ensure_client_compatible(Some(env!("CARGO_PKG_VERSION"))).is_ok());
        let error = ensure_client_compatible(Some("9999.0.0"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("upgrade MUR"), "{error}");
    }

    #[test]
    fn fleets_reject_model_flags_and_otherwise_remain_unchanged() {
        assert!(
            decide_install(
                "fleets/x",
                Some(OfficialModelPolicy::CostFirst),
                None,
                vec![],
                true,
            )
            .is_err()
        );
        assert_eq!(
            decide_install("fleets/x", None, None, vec![], false).unwrap(),
            InstallDecision::Select(ModelSelection::Unchanged)
        );
    }
}
