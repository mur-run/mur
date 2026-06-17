//! Tauri commands backing the Model Library surface. Thin wrappers over
//! `mur_core::model_discovery` + `mur_core::model_prices`, writing the
//! shared `~/.mur/models.yaml` registry. Secrets are stored as SecretRef
//! and never returned to the UI.

use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::route::RouteTier;
use mur_common::secret::SecretRef;
use mur_core::model_discovery::{self, default_alias};
use mur_core::model_prices::{self, PriceInfo};
use serde::{Deserialize, Serialize};

const PROBE_TIMEOUT_SECS: u64 = 3;
const DISCOVER_TIMEOUT_SECS: u64 = 10;
/// Keychain service name for stored provider keys.
const KEYCHAIN_SERVICE: &str = "mur";

#[derive(Serialize)]
pub struct EnrichedModelView {
    pub model: String,
    pub alias: String,
    pub input_cost: Option<f64>,
    pub output_cost: Option<f64>,
    pub context_window: Option<u64>,
}

impl EnrichedModelView {
    pub fn build(provider: &str, model: &str, price: Option<PriceInfo>) -> Self {
        Self {
            alias: default_alias(provider, model),
            input_cost: price.as_ref().map(|p| p.input_per_1k),
            output_cost: price.as_ref().map(|p| p.output_per_1k),
            context_window: price.as_ref().and_then(|p| p.context_window),
            model: model.to_string(),
        }
    }
}

#[derive(Serialize)]
pub struct DetectedLocalView {
    pub key: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<EnrichedModelView>,
}

/// Build a SecretRef from a UI kind + value. Empty value → None.
pub fn build_secret(kind: &str, value: &str) -> Option<SecretRef> {
    if value.is_empty() {
        return None;
    }
    match kind {
        "env" => Some(SecretRef::Env(value.to_string())),
        "file" => value.parse::<SecretRef>().ok(),
        _ => Some(SecretRef::Keychain {
            service: KEYCHAIN_SERVICE.to_string(),
            account: value.to_string(),
        }),
    }
}

fn mur_home() -> std::path::PathBuf {
    ModelRegistry::default_path()
        .ok()
        .and_then(|p| p.parent().map(|x| x.to_path_buf()))
        .unwrap_or_default()
}

#[derive(Deserialize)]
pub struct Pick {
    pub model: String,
    pub alias: String,
}

#[tauri::command]
pub fn probe_local_providers() -> Result<Vec<DetectedLocalView>, String> {
    let home = mur_home();
    let detected = model_discovery::probe_local(PROBE_TIMEOUT_SECS);
    Ok(detected
        .into_iter()
        .map(|d| DetectedLocalView {
            models: d
                .models
                .iter()
                .map(|m| {
                    let price = model_prices::lookup(&home, &d.key, m, true);
                    EnrichedModelView::build(&d.key, m, price)
                })
                .collect(),
            key: d.key,
            name: d.name,
            base_url: d.base_url,
        })
        .collect())
}

#[tauri::command]
pub fn test_provider(
    provider: String,
    base_url: String,
    api_key: String,
) -> Result<Vec<EnrichedModelView>, String> {
    let home = mur_home();
    let key = (!api_key.is_empty()).then_some(api_key.as_str());
    let ids = model_discovery::discover_models(&base_url, key, DISCOVER_TIMEOUT_SECS)
        .map_err(|e| format!("discovery failed: {e}"))?;
    Ok(ids
        .into_iter()
        .map(|m| {
            let price = model_prices::lookup(&home, &provider, &m, false);
            EnrichedModelView::build(&provider, &m, price)
        })
        .collect())
}

#[tauri::command]
pub fn add_models(
    provider: String,
    base_url: String,
    tier: String,
    secret_kind: String,
    secret_value: String,
    picks: Vec<Pick>,
) -> Result<(), String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    let home = mur_home();
    let route_tier = if tier == "local" {
        RouteTier::Local
    } else {
        RouteTier::Frontier
    };
    let secret = build_secret(&secret_kind, &secret_value);
    let is_local = route_tier == RouteTier::Local;
    for pick in picks {
        let price = model_prices::lookup(&home, &provider, &pick.model, is_local);
        let entry = ModelEntry {
            provider: provider.clone(),
            model: pick.model.clone(),
            base_url: Some(base_url.clone()),
            secret: secret.clone(),
            tier: Some(route_tier),
            input_cost_per_1k: price.as_ref().map(|p| p.input_per_1k),
            output_cost_per_1k: price.as_ref().map(|p| p.output_per_1k),
            context_window: price.as_ref().and_then(|p| p.context_window),
            ..Default::default()
        };
        // Non-destructive: only insert if the alias is new.
        reg.models.entry(pick.alias).or_insert(entry);
    }
    reg.save_to(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_model(ref_name: String) -> Result<(), String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    reg.models.remove(&ref_name);
    reg.save_to(&path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enriched_view_carries_pricing_and_alias() {
        let v = EnrichedModelView::build(
            "openai",
            "gpt-5.2",
            Some(mur_core::model_prices::PriceInfo {
                input_per_1k: 0.00125,
                output_per_1k: 0.01,
                context_window: Some(400_000),
            }),
        );
        assert_eq!(v.alias, "openai_gpt_5_2");
        assert_eq!(v.input_cost, Some(0.00125));
        assert_eq!(v.output_cost, Some(0.01));
        assert_eq!(v.context_window, Some(400_000));
    }

    #[test]
    fn secret_ref_built_from_kind() {
        assert!(matches!(
            build_secret("env", "OPENAI_API_KEY"),
            Some(SecretRef::Env(_))
        ));
        assert!(matches!(
            build_secret("keychain", "sk-xxx"),
            Some(SecretRef::Keychain { .. })
        ));
        assert!(build_secret("keychain", "").is_none()); // empty → no secret
    }
}
