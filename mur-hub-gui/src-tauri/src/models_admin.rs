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
use tauri::{AppHandle, Manager};

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

/// Find the stored `SecretRef` to reuse when re-exploring an already-connected
/// provider: the first registry entry for `provider` that carries a secret.
/// Returns `None` for providers with no stored credential (e.g. local).
pub fn provider_secret_ref(reg: &ModelRegistry, provider: &str) -> Option<SecretRef> {
    reg.models
        .values()
        .find(|e| e.provider == provider && e.secret.is_some())
        .and_then(|e| e.secret.clone())
}

#[tauri::command]
pub async fn test_provider(
    provider: String,
    base_url: String,
    api_key: Option<String>,
) -> Result<Vec<EnrichedModelView>, String> {
    // Explicit key from the UI wins. Otherwise (re-exploring a connected
    // provider with no key field) resolve the provider's stored SecretRef so
    // re-explore no longer 401s.
    let key: Option<String> = match api_key.filter(|k| !k.is_empty()) {
        Some(k) => Some(k),
        None => {
            let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
            match ModelRegistry::load_from(&path)
                .ok()
                .and_then(|reg| provider_secret_ref(&reg, &provider))
            {
                Some(secret) => secret.resolve_to_string().await,
                None => None,
            }
        }
    };

    let home = mur_home();
    let base = base_url.clone();
    let prov = provider.clone();
    let prov2 = provider.clone();
    // discover_models uses reqwest::blocking — run it off the async runtime.
    let ids = tokio::task::spawn_blocking(move || {
        model_discovery::discover_models_for(&prov2, &base, key.as_deref(), DISCOVER_TIMEOUT_SECS)
    })
    .await
    .map_err(|e| format!("discovery task failed: {e}"))?
    .map_err(|e| format!("discovery failed: {e}"))?;

    Ok(ids
        .into_iter()
        .map(|m| {
            let price = model_prices::lookup(&home, &prov, &m, false);
            EnrichedModelView::build(&prov, &m, price)
        })
        .collect())
}

#[tauri::command]
pub fn add_models(
    provider: String,
    base_url: Option<String>,
    tier: String,
    secret_kind: String,
    secret_value: Option<String>,
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
    let secret = build_secret(&secret_kind, secret_value.as_deref().unwrap_or(""));
    let is_local = route_tier == RouteTier::Local;
    // Resolve base_url: use the provided value, or fall back to an existing
    // registry entry for the same provider (handles connected providers whose
    // base_url was already stored), or leave None.
    let resolved_base_url = base_url.clone().or_else(|| {
        reg.models
            .values()
            .find(|e| e.provider == provider)
            .and_then(|e| e.base_url.clone())
    });
    for pick in picks {
        let price = model_prices::lookup(&home, &provider, &pick.model, is_local);
        let entry = ModelEntry {
            provider: provider.clone(),
            model: pick.model.clone(),
            base_url: resolved_base_url.clone(),
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

/// Overwrite the cost-per-1k (USD) of an existing registry entry in place.
/// Split out from the `update_model` command so it's testable without disk I/O.
pub fn apply_cost_update(
    reg: &mut ModelRegistry,
    ref_name: &str,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
) -> Result<(), String> {
    let entry = reg
        .models
        .get_mut(ref_name)
        .ok_or_else(|| format!("model '{ref_name}' not found in registry"))?;
    entry.input_cost_per_1k = input_cost;
    entry.output_cost_per_1k = output_cost;
    Ok(())
}

/// Edit the cost-per-1k (USD) of an existing registry entry. Used by the
/// Model Library / Settings "edit" affordance — discovery already fills
/// these from the models.dev catalog, but operators override stale or
/// missing prices here.
#[tauri::command]
pub fn update_model(
    ref_name: String,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
) -> Result<(), String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    apply_cost_update(&mut reg, &ref_name, input_cost, output_cost)?;
    reg.save_to(&path).map_err(|e| e.to_string())
}

/// Switch the concierge to a registry model, restarting the sidecar.
#[tauri::command]
pub async fn use_registry_model(app: AppHandle, ref_name: String) -> Result<(), String> {
    let home = crate::mur_home_path();
    crate::seed_mur::set_concierge_model_ref(&home, &ref_name).map_err(|e| e.to_string())?;

    let supervisor = app.state::<crate::SupervisorState>();
    supervisor.0.stop("mur").await;
    if let Err(e) = supervisor.0.start("mur").await {
        tracing::warn!("restart concierge after model change failed: {e}");
    }
    Ok(())
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

    #[test]
    fn provider_secret_ref_picks_stored_credential() {
        let mut reg = ModelRegistry::default();
        // anthropic entry with a keychain secret
        reg.models.insert(
            "claude_opus".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-opus-4-8".into(),
                secret: Some(SecretRef::Keychain {
                    service: "mur".into(),
                    account: "anthropic".into(),
                }),
                ..Default::default()
            },
        );
        // local entry with no secret
        reg.models.insert(
            "ollama_llama3".into(),
            ModelEntry {
                provider: "ollama".into(),
                model: "llama3".into(),
                tier: Some(RouteTier::Local),
                ..Default::default()
            },
        );

        // connected provider → returns its stored SecretRef
        assert!(matches!(
            provider_secret_ref(&reg, "anthropic"),
            Some(SecretRef::Keychain { .. })
        ));
        // local / keyless provider → None
        assert_eq!(provider_secret_ref(&reg, "ollama"), None);
        // unknown provider → None
        assert_eq!(provider_secret_ref(&reg, "openai"), None);
    }

    #[test]
    fn apply_cost_update_overwrites_existing_entry() {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "openai_gpt5".into(),
            ModelEntry {
                provider: "openai".into(),
                model: "gpt-5.2".into(),
                input_cost_per_1k: Some(0.001),
                output_cost_per_1k: Some(0.01),
                ..Default::default()
            },
        );

        apply_cost_update(&mut reg, "openai_gpt5", Some(0.002), None).unwrap();

        let entry = &reg.models["openai_gpt5"];
        assert_eq!(entry.input_cost_per_1k, Some(0.002));
        assert_eq!(entry.output_cost_per_1k, None);
    }

    #[test]
    fn apply_cost_update_errors_on_unknown_ref() {
        let mut reg = ModelRegistry::default();
        assert!(apply_cost_update(&mut reg, "missing", Some(0.001), None).is_err());
    }
}
