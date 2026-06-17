//! models.dev price catalog: pure parse + lookup, plus a best-effort cached
//! fetch. Prices in the catalog are per 1,000,000 tokens; we convert to the
//! per-1k unit the registry uses (divide by 1000).

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;

/// models.dev catalog endpoint (no auth required to read).
#[allow(dead_code)]
pub const CATALOG_URL: &str = "https://models.dev/api.json";
/// Cache filename under `~/.mur/cache/`.
#[allow(dead_code)]
pub const CACHE_FILE: &str = "model-prices.json";
/// Catalog cache freshness window.
#[allow(dead_code)]
pub const TTL_HOURS: u64 = 168; // 7 days
/// Per-request network timeout.
#[allow(dead_code)]
pub const TIMEOUT_SECS: u64 = 10; // used by Task 2 (fetch/cache)

/// Resolved pricing for one model, in the registry's per-1k unit.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub struct PriceInfo {
    pub input_per_1k: f64,
    pub output_per_1k: f64,
    pub context_window: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RawProvider {
    #[serde(default)]
    models: HashMap<String, RawModel>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RawModel {
    #[serde(default)]
    cost: Option<RawCost>,
    #[serde(default)]
    limit: Option<RawLimit>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RawCost {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RawLimit {
    #[serde(default)]
    context: Option<u64>,
}

/// Parsed catalog: provider → (model → raw). Lookups apply unit conversion.
#[allow(dead_code)]
pub struct Catalog {
    providers: HashMap<String, RawProvider>,
}

/// Parse the models.dev JSON document.
#[allow(dead_code)]
pub fn parse_catalog(json: &str) -> Result<Catalog> {
    let providers: HashMap<String, RawProvider> = serde_json::from_str(json)?;
    Ok(Catalog { providers })
}

#[allow(dead_code)]
const PER_MILLION: f64 = 1_000_000.0 / 1_000.0; // = 1000: per-1M ÷ 1000 = per-1k

impl Catalog {
    /// Resolve `(provider, model)` to per-1k pricing. Matching order:
    /// exact → case-insensitive model → provider-namespaced id
    /// (`vendor/model`) resolved against the embedded vendor + model.
    #[allow(dead_code)]
    pub fn lookup(&self, provider: &str, model: &str) -> Option<PriceInfo> {
        if let Some(p) = self.lookup_in(provider, model) {
            return Some(p);
        }
        // Namespaced id like "anthropic/claude-opus-4-8" (OpenRouter style).
        if let Some((vendor, sub)) = model.split_once('/') {
            if let Some(p) = self.lookup_in(vendor, sub) {
                return Some(p);
            }
        }
        None
    }

    #[allow(dead_code)]
    fn lookup_in(&self, provider: &str, model: &str) -> Option<PriceInfo> {
        let prov = self.providers.get(provider)?;
        // exact, then case-insensitive
        let raw = prov.models.get(model).or_else(|| {
            prov.models
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(model))
                .map(|(_, v)| v)
        })?;
        let cost = raw.cost.as_ref()?;
        Some(PriceInfo {
            input_per_1k: cost.input? / PER_MILLION,
            output_per_1k: cost.output? / PER_MILLION,
            context_window: raw.limit.as_ref().and_then(|l| l.context),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "anthropic": {
        "id": "anthropic",
        "models": {
          "claude-opus-4-8": { "cost": { "input": 5, "output": 25 }, "limit": { "context": 200000 } }
        }
      },
      "deepseek": {
        "id": "deepseek",
        "models": {
          "deepseek-v4-flash": { "cost": { "input": 0.07, "output": 0.28 }, "limit": { "context": 128000 } }
        }
      }
    }"#;

    #[test]
    fn parses_and_converts_per_million_to_per_1k() {
        let cat = parse_catalog(FIXTURE).unwrap();
        let p = cat.lookup("anthropic", "claude-opus-4-8").unwrap();
        // 5 per 1M  → 0.005 per 1k ; 25 per 1M → 0.025 per 1k
        assert!((p.input_per_1k - 0.005).abs() < 1e-12);
        assert!((p.output_per_1k - 0.025).abs() < 1e-12);
        assert_eq!(p.context_window, Some(200_000));
    }

    #[test]
    fn lookup_miss_returns_none() {
        let cat = parse_catalog(FIXTURE).unwrap();
        assert!(cat.lookup("anthropic", "nonexistent-model").is_none());
        assert!(cat.lookup("no-such-provider", "x").is_none());
    }

    #[test]
    fn lookup_is_case_insensitive_on_model() {
        let cat = parse_catalog(FIXTURE).unwrap();
        assert!(cat.lookup("anthropic", "Claude-Opus-4-8").is_some());
    }

    #[test]
    fn lookup_matches_provider_namespaced_id() {
        // OpenRouter-style id "anthropic/claude-opus-4-8" under provider "openrouter"
        let cat = parse_catalog(FIXTURE).unwrap();
        let p = cat.lookup("openrouter", "anthropic/claude-opus-4-8");
        assert!(p.is_some(), "namespaced id should fall back to embedded provider/model");
    }
}
