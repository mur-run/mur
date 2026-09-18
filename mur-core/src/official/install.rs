//! Install one item from the official catalog.
//!
//! Both CLI and Hub call this module. It owns the trust boundary and the
//! transaction: verify the account-bound license and signed package, plan from
//! the local registry, import, revalidate the registry, then atomically save the
//! configured profile. It deliberately prints nothing.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use mur_common::{
    agent::AgentProfile,
    config::{RoutingOverride, SmartOverride},
    model::ModelRegistry,
    muragent::{reader::MuragentArchive, validator},
    official::{LicenseCheck, check_license},
    skill::publisher_trust::MUR_OFFICIAL_LICENSE_KEY_FP,
};
use serde::{Deserialize, Serialize};

use crate::official::{
    client::download_item,
    model_selection::{
        ModelSelectionPlan, ModelSelectionPolicy, plan_model_selection, validate_explicit_selection,
    },
    store::save_license,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OfficialInstallOptions {
    pub model_selection: ModelSelection,
}

impl Default for OfficialInstallOptions {
    fn default() -> Self {
        Self {
            model_selection: ModelSelection::Unchanged,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelSelection {
    Automatic(ModelSelectionPolicy),
    Explicit {
        primary: String,
        fallbacks: Vec<String>,
    },
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallOutcome {
    pub item_id: String,
    pub agent_name: Option<String>,
    pub model_selection: Option<ModelSelectionPlan>,
}

/// Download and install `id` (`agents/<name>` or `fleets/<name>`).
pub async fn install_item(id: &str, options: &OfficialInstallOptions) -> Result<InstallOutcome> {
    let tokens =
        crate::auth::load_tokens().context("not logged in — run `mur auth login` first")?;
    let user_id = tokens
        .user_id
        .clone()
        .context("stored login has no account id — run `mur auth logout` then `mur auth login`")?;

    let base = crate::auth::server_url();
    let client = reqwest::Client::new();
    let (bytes, license) = download_item(&client, &base, &tokens.access_token, id).await?;

    match check_license(&license, id, &user_id, MUR_OFFICIAL_LICENSE_KEY_FP) {
        LicenseCheck::Ok => {}
        other => bail!("server returned an invalid license ({other:?}) — refusing install"),
    }

    let mur_home = crate::paths::mur_root(None);
    install_verified_bytes(&mur_home, id, &bytes, &license, options)
}

fn install_verified_bytes(
    mur_home: &Path,
    id: &str,
    bytes: &[u8],
    license: &mur_common::official::OfficialLicense,
    options: &OfficialInstallOptions,
) -> Result<InstallOutcome> {
    let (kind, catalog_name) = id
        .split_once('/')
        .filter(|(_, name)| !name.is_empty())
        .with_context(|| {
            format!("unknown catalog id '{id}' — expected agents/<name> or fleets/<name>")
        })?;

    if kind == "fleets" {
        ensure!(
            options.model_selection == ModelSelection::Unchanged,
            "model selection options apply only to Agents with signed model requirements"
        );
        save_license(mur_home, license)?;
        let dir = tempfile::tempdir().context("temp dir")?;
        let path = dir.path().join(format!("{catalog_name}.fleet"));
        fs::write(&path, bytes).context("write bundle")?;
        crate::cmd::fleet::import::cmd_fleet_import(
            mur_home,
            &path,
            crate::cmd::fleet::import::ImportOpts::default(),
        )?;
        return Ok(InstallOutcome {
            item_id: id.to_string(),
            agent_name: None,
            model_selection: None,
        });
    }
    ensure!(kind == "agents", "unknown catalog kind '{kind}'");

    // Validate the signed manifest before any Agent mutation. Requirements in
    // the catalog are presentation data; only this signed manifest is authority.
    let archive = MuragentArchive::read_from_bytes(bytes).context("parse .muragent bundle")?;
    let validation = validator::validate(&archive).context("validate signed .muragent")?;
    ensure!(
        validation.manifest.agent.slug == catalog_name,
        "catalog id '{id}' does not match signed agent slug '{}'",
        validation.manifest.agent.slug
    );

    let requirements = validation.manifest.model_requirements.as_ref();
    let plan = match (requirements, &options.model_selection) {
        (None, ModelSelection::Unchanged) => None,
        (None, _) => bail!(
            "agent '{catalog_name}' has no signed model requirements; model selection options are not allowed"
        ),
        (Some(_), ModelSelection::Unchanged) => {
            bail!("agent '{catalog_name}' requires a model selection policy or explicit model refs")
        }
        (Some(requirements), selection) => {
            let registry = ModelRegistry::load_from(&mur_home.join("models.yaml"))
                .context("load model registry")?;
            Some(plan_selection(&registry, requirements, selection)?)
        }
    };

    // License persistence is intentionally outside rollback; the approved trust
    // contract permits keeping a valid license after a failed import.
    save_license(mur_home, license)?;
    let mut rollback = AgentRollback::capture(
        mur_home
            .join("agents")
            .join(&validation.manifest.agent.slug),
    )?;
    let result = (|| {
        let dir = tempfile::tempdir().context("temp dir")?;
        let path = dir.path().join(format!("{catalog_name}.muragent"));
        fs::write(&path, bytes).context("write package")?;
        let resolution = if requirements.is_some() {
            crate::cmd::agent::install::ResolveModelAfterInstall::Suppress
        } else {
            crate::cmd::agent::install::ResolveModelAfterInstall::ExistingWizard
        };
        let (installed_name, _) =
            crate::cmd::agent::install::cmd_install_with_resolution(&path, None, None, resolution)?;

        if let (Some(requirements), Some(initial_plan)) = (requirements, plan.as_ref()) {
            configure_installed_agent(
                mur_home,
                &installed_name,
                requirements,
                initial_plan,
                &options.model_selection,
            )?;
        }
        Ok(validation.manifest.agent.slug.clone())
    })();

    match result {
        Ok(agent_name) => {
            rollback.disarm();
            Ok(InstallOutcome {
                item_id: id.to_string(),
                agent_name: Some(agent_name),
                model_selection: plan,
            })
        }
        Err(error) => {
            rollback
                .restore()
                .context("restore Agent after failed official install")?;
            Err(error)
        }
    }
}

fn plan_selection(
    registry: &ModelRegistry,
    requirements: &mur_common::muragent::manifest::ModelRequirements,
    selection: &ModelSelection,
) -> Result<ModelSelectionPlan> {
    match selection {
        ModelSelection::Automatic(policy) => {
            plan_model_selection(registry, requirements, *policy, Utc::now())
        }
        ModelSelection::Explicit { primary, fallbacks } => {
            validate_explicit_selection(registry, requirements, primary, fallbacks, Utc::now())
        }
        ModelSelection::Unchanged => bail!("model selection is required"),
    }
}

fn configure_installed_agent(
    mur_home: &Path,
    name: &str,
    requirements: &mur_common::muragent::manifest::ModelRequirements,
    initial_plan: &ModelSelectionPlan,
    selection: &ModelSelection,
) -> Result<()> {
    // Re-read at the last possible moment. A model removed between preview and
    // commit must fail and trigger directory rollback, not leave dangling refs.
    let registry = ModelRegistry::load_from(&mur_home.join("models.yaml"))
        .context("re-read model registry before profile commit")?;
    let revalidated = validate_explicit_selection(
        &registry,
        requirements,
        &initial_plan.primary,
        &initial_plan.fallbacks,
        Utc::now(),
    )?;
    let entry = registry
        .models
        .get(&revalidated.primary)
        .context("selected primary disappeared from model registry")?;

    let profile_path = mur_home.join("agents").join(name).join("profile.yaml");
    let mut profile: AgentProfile = serde_yaml_ng::from_str(
        &fs::read_to_string(&profile_path)
            .with_context(|| format!("read {}", profile_path.display()))?,
    )
    .with_context(|| format!("parse {}", profile_path.display()))?;
    profile.model_ref = Some(revalidated.primary);
    profile.fallback_chain = revalidated.fallbacks;
    profile.model.provider = entry.provider.clone();
    profile.model.name = entry.model.clone();
    if matches!(
        selection,
        ModelSelection::Automatic(ModelSelectionPolicy::PrivacyFirst)
    ) {
        profile.routing = Some(RoutingOverride {
            enabled: Some(false),
            ..Default::default()
        });
        profile.smart = Some(SmartOverride {
            enabled: Some(false),
            ..Default::default()
        });
    }
    crate::cmd::agent::save_profile(&profile_path, &mut profile)
}

struct AgentRollback {
    target: PathBuf,
    backup_root: tempfile::TempDir,
    existed: bool,
    armed: bool,
}

impl AgentRollback {
    fn capture(target: PathBuf) -> Result<Self> {
        let backup_root = tempfile::tempdir().context("create Agent rollback directory")?;
        let existed = target.exists();
        if existed {
            copy_dir_all(&target, &backup_root.path().join("agent"))?;
        }
        Ok(Self {
            target,
            backup_root,
            existed,
            armed: true,
        })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn restore(&mut self) -> Result<()> {
        if !self.armed {
            return Ok(());
        }
        if self.target.exists() {
            fs::remove_dir_all(&self.target)
                .with_context(|| format!("remove {}", self.target.display()))?;
        }
        if self.existed {
            copy_dir_all(&self.backup_root.path().join("agent"), &self.target)?;
        }
        self.armed = false;
        Ok(())
    }
}

impl Drop for AgentRollback {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = self.restore()
        {
            tracing::error!(%error, path = %self.target.display(), "failed to roll back Agent install");
        }
    }
}

fn copy_dir_all(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination).with_context(|| format!("create {}", destination.display()))?;
    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_all(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "copy {} -> {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        } else if file_type.is_symlink() {
            bail!("refusing to snapshot symlink {}", source_path.display());
        }
    }
    Ok(())
}

/// The agent name an `agents/<name>` catalog id installs as, if it is one.
pub fn installed_agent_name(id: &str) -> Option<&str> {
    match id.split_once('/') {
        Some(("agents", name)) if !name.is_empty() => Some(name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_ids_yield_a_name_others_do_not() {
        assert_eq!(
            installed_agent_name("agents/researcher"),
            Some("researcher")
        );
        assert_eq!(installed_agent_name("fleets/newsroom"), None);
        assert_eq!(installed_agent_name("agents/"), None);
        assert_eq!(installed_agent_name("researcher"), None);
    }

    #[test]
    fn rollback_removes_fresh_install() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("agent");
        let mut guard = AgentRollback::capture(target.clone()).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("new"), b"new").unwrap();
        guard.restore().unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn rollback_restores_existing_tree_byte_for_byte() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("agent");
        fs::create_dir_all(target.join("nested")).unwrap();
        fs::write(target.join("profile.yaml"), b"old-profile\n").unwrap();
        fs::write(target.join("nested/data"), [0, 1, 2, 255]).unwrap();
        let mut guard = AgentRollback::capture(target.clone()).unwrap();
        fs::remove_dir_all(&target).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("profile.yaml"), b"new-profile\n").unwrap();
        guard.restore().unwrap();
        assert_eq!(
            fs::read(target.join("profile.yaml")).unwrap(),
            b"old-profile\n"
        );
        assert_eq!(
            fs::read(target.join("nested/data")).unwrap(),
            [0, 1, 2, 255]
        );
    }

    fn requirements() -> mur_common::muragent::manifest::ModelRequirements {
        mur_common::muragent::manifest::ModelRequirements {
            chat: true,
            tools: true,
            minimum_context_window: None,
        }
    }

    fn registry_with_local_and_cloud() -> ModelRegistry {
        use mur_common::model::{BillingMode, ModelEntry};
        let mut registry = ModelRegistry::default();
        registry.models.insert(
            "local".into(),
            ModelEntry {
                provider: "ollama".into(),
                model: "qwen3".into(),
                capabilities: vec!["chat".into(), "tools".into()],
                billing: Some(BillingMode::Local),
                ..Default::default()
            },
        );
        registry.models.insert(
            "cloud".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude".into(),
                capabilities: vec!["chat".into(), "tools".into()],
                billing: Some(BillingMode::UsageBilled),
                ..Default::default()
            },
        );
        registry
    }

    #[test]
    fn explicit_selection_rejects_missing_or_ineligible_refs() {
        let registry = registry_with_local_and_cloud();
        let missing = ModelSelection::Explicit {
            primary: "missing".into(),
            fallbacks: vec![],
        };
        assert!(plan_selection(&registry, &requirements(), &missing).is_err());

        let mut strict = requirements();
        strict.minimum_context_window = Some(8_192);
        let ineligible = ModelSelection::Explicit {
            primary: "local".into(),
            fallbacks: vec![],
        };
        assert!(plan_selection(&registry, &strict, &ineligible).is_err());
    }

    #[test]
    fn configuring_profile_sets_chain_and_privacy_overrides_without_registry_write() {
        let home = tempfile::tempdir().unwrap();
        let registry = registry_with_local_and_cloud();
        let registry_path = home.path().join("models.yaml");
        registry.save_to(&registry_path).unwrap();
        let original_registry = fs::read(&registry_path).unwrap();

        let agent_dir = home.path().join("agents/orchestrator");
        fs::create_dir_all(&agent_dir).unwrap();
        let mut profile = AgentProfile::default_for_tests();
        profile.name = "orchestrator".into();
        fs::write(
            agent_dir.join("profile.yaml"),
            serde_yaml_ng::to_string(&profile).unwrap(),
        )
        .unwrap();
        let plan = ModelSelectionPlan {
            primary: "local".into(),
            fallbacks: vec![],
            warnings: vec![],
        };

        configure_installed_agent(
            home.path(),
            "orchestrator",
            &requirements(),
            &plan,
            &ModelSelection::Automatic(ModelSelectionPolicy::PrivacyFirst),
        )
        .unwrap();

        let saved: AgentProfile =
            serde_yaml_ng::from_str(&fs::read_to_string(agent_dir.join("profile.yaml")).unwrap())
                .unwrap();
        assert_eq!(saved.model_ref.as_deref(), Some("local"));
        assert!(saved.fallback_chain.is_empty());
        assert_eq!(saved.model.provider, "ollama");
        assert_eq!(saved.model.name, "qwen3");
        assert_eq!(saved.routing.and_then(|value| value.enabled), Some(false));
        assert_eq!(saved.smart.and_then(|value| value.enabled), Some(false));
        assert_eq!(fs::read(&registry_path).unwrap(), original_registry);
    }

    #[test]
    fn final_revalidation_rejects_a_removed_primary_without_touching_profile() {
        let home = tempfile::tempdir().unwrap();
        let registry_path = home.path().join("models.yaml");
        registry_with_local_and_cloud()
            .save_to(&registry_path)
            .unwrap();
        let agent_dir = home.path().join("agents/orchestrator");
        fs::create_dir_all(&agent_dir).unwrap();
        let profile = AgentProfile::default_for_tests();
        let original_profile = serde_yaml_ng::to_string(&profile).unwrap();
        fs::write(agent_dir.join("profile.yaml"), &original_profile).unwrap();

        ModelRegistry::default().save_to(&registry_path).unwrap();
        let plan = ModelSelectionPlan {
            primary: "local".into(),
            fallbacks: vec![],
            warnings: vec![],
        };
        let result = configure_installed_agent(
            home.path(),
            "orchestrator",
            &requirements(),
            &plan,
            &ModelSelection::Automatic(ModelSelectionPolicy::PrivacyFirst),
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(agent_dir.join("profile.yaml")).unwrap(),
            original_profile
        );
    }
}
