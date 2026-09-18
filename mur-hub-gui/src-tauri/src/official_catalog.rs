//! Hub-side bridge to the official catalog on app.mur.run.
//!
//! Preview is deliberately advisory: install downloads the signed package and
//! mur-core replans/revalidates against the current registry before mutation.

use chrono::Utc;
use mur_common::{model::ModelRegistry, muragent::manifest::ModelRequirements};
use mur_core::official::{
    install::{InstallOutcome, ModelSelection, OfficialInstallOptions},
    model_selection::{ModelSelectionPlan, ModelSelectionPolicy, plan_model_selection},
};
use serde::Serialize;

#[derive(Serialize)]
pub struct CatalogItemView {
    pub id: String,
    pub tier: String,
    pub version: String,
    pub description: String,
    pub agent_name: Option<String>,
    pub model_requirements: Option<ModelRequirements>,
    pub min_mur_version: Option<String>,
    pub compatible: bool,
    pub compatibility_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InstallOutcomeView {
    pub item_id: String,
    pub agent_name: Option<String>,
    pub model_selection: Option<ModelSelectionPlan>,
}

impl From<InstallOutcome> for InstallOutcomeView {
    fn from(value: InstallOutcome) -> Self {
        Self {
            item_id: value.item_id,
            agent_name: value.agent_name,
            model_selection: value.model_selection,
        }
    }
}

fn compatibility(minimum: Option<&str>) -> (bool, Option<String>) {
    match mur_core::official::client::ensure_client_compatible(minimum) {
        Ok(()) => (true, None),
        Err(error) => (false, Some(error.to_string())),
    }
}

/// Public listing — no auth. Errors surface the server's own message.
#[tauri::command]
pub async fn official_list() -> Result<Vec<CatalogItemView>, String> {
    let base = mur_core::auth::server_url();
    let items = mur_core::official::client::fetch_catalog(&reqwest::Client::new(), &base)
        .await
        .map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .map(|item| {
            let (compatible, compatibility_error) = compatibility(item.min_mur_version.as_deref());
            CatalogItemView {
                agent_name: mur_core::official::install::installed_agent_name(&item.id)
                    .map(str::to_string),
                id: item.id,
                tier: item.tier,
                version: item.version,
                description: item.description,
                model_requirements: item.model_requirements,
                min_mur_version: item.min_mur_version,
                compatible,
                compatibility_error,
            }
        })
        .collect())
}

#[tauri::command]
pub fn official_logged_in() -> bool {
    mur_core::auth::load_tokens().is_some()
}

fn plan_for_registry(
    registry: &ModelRegistry,
    requirements: &ModelRequirements,
    policy: ModelSelectionPolicy,
) -> Result<ModelSelectionPlan, String> {
    plan_model_selection(registry, requirements, policy, Utc::now()).map_err(|e| e.to_string())
}

async fn catalog_item(id: &str) -> Result<mur_core::official::client::CatalogItem, String> {
    let base = mur_core::auth::server_url();
    mur_core::official::client::fetch_catalog(&reqwest::Client::new(), &base)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("official catalog item '{id}' not found"))
}

/// Preview a policy against the current registry. The catalog requirements are
/// display input only; install validates the signed package independently.
#[tauri::command]
pub async fn official_plan_models(
    id: String,
    policy: ModelSelectionPolicy,
) -> Result<ModelSelectionPlan, String> {
    let item = catalog_item(&id).await?;
    mur_core::official::client::ensure_client_compatible(item.min_mur_version.as_deref())
        .map_err(|e| e.to_string())?;
    let requirements = item
        .model_requirements
        .ok_or_else(|| format!("official catalog item '{id}' has no model requirements"))?;
    let registry = ModelRegistry::load_from(&mur_core::paths::mur_root(None).join("models.yaml"))
        .map_err(|e| format!("load model registry: {e}"))?;
    plan_for_registry(&registry, &requirements, policy)
}

/// Download and install one item. mur-core revalidates the signed requirements
/// and registry at commit time; the preview is never trusted here.
#[tauri::command]
pub async fn official_install(
    id: String,
    model_selection: ModelSelection,
) -> Result<InstallOutcomeView, String> {
    let item = catalog_item(&id).await?;
    mur_core::official::client::ensure_client_compatible(item.min_mur_version.as_deref())
        .map_err(|e| e.to_string())?;
    mur_core::official::install::install_item(&id, &OfficialInstallOptions { model_selection })
        .await
        .map(Into::into)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::model::ModelEntry;

    fn registry() -> ModelRegistry {
        let mut registry = ModelRegistry::default();
        registry.models.insert(
            "local".into(),
            ModelEntry {
                provider: "ollama".into(),
                model: "qwen".into(),
                capabilities: vec!["chat".into(), "tools".into()],
                context_window: Some(32_000),
                ..Default::default()
            },
        );
        registry
    }

    fn requirements() -> ModelRequirements {
        ModelRequirements {
            chat: true,
            tools: true,
            minimum_context_window: Some(16_000),
        }
    }

    #[test]
    fn official_policy_dto_accepts_kebab_case_and_plans_deterministically() {
        let policy: ModelSelectionPolicy = serde_json::from_str("\"privacy-first\"").unwrap();
        assert_eq!(policy, ModelSelectionPolicy::PrivacyFirst);
        let first = plan_for_registry(&registry(), &requirements(), policy).unwrap();
        let second = plan_for_registry(&registry(), &requirements(), policy).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn official_compatibility_rejects_newer_minimum() {
        let (compatible, error) = compatibility(Some("9999.0.0"));
        assert!(!compatible);
        assert!(error.unwrap().contains("upgrade MUR"));
    }

    #[test]
    fn official_install_selection_accepts_kebab_case_policy() {
        let selection: ModelSelection =
            serde_json::from_str(r#"{"Automatic":"cost-first"}"#).unwrap();
        assert_eq!(
            selection,
            ModelSelection::Automatic(ModelSelectionPolicy::CostFirst)
        );
    }
}
