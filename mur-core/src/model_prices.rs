//! models.dev price catalog: pure parse + lookup, plus a best-effort cached
//! fetch. Prices in the catalog are per 1,000,000 tokens; we convert to the
//! per-1k unit the registry uses (divide by 1000).

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// models.dev catalog endpoint (no auth required to read).
pub const CATALOG_URL: &str = "https://models.dev/api.json";
/// Cache filename under `~/.mur/cache/`.
pub const CACHE_FILE: &str = "model-prices.json";
/// Catalog cache freshness window.
pub const TTL_HOURS: u64 = 168; // 7 days
/// Per-request network timeout.
pub const TIMEOUT_SECS: u64 = 10; // used by Task 2 (fetch/cache)

/// Resolved pricing for one model, in the registry's per-1k unit.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceInfo {
    pub input_per_1k: f64,
    pub output_per_1k: f64,
    pub context_window: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawProvider {
    #[serde(default)]
    models: HashMap<String, RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    #[serde(default)]
    cost: Option<RawCost>,
    #[serde(default)]
    limit: Option<RawLimit>,
}

#[derive(Debug, Deserialize)]
struct RawCost {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawLimit {
    #[serde(default)]
    context: Option<u64>,
}

/// Parsed catalog: provider → (model → raw). Lookups apply unit conversion.
pub struct Catalog {
    providers: HashMap<String, RawProvider>,
}

/// Parse the models.dev JSON document.
pub fn parse_catalog(json: &str) -> Result<Catalog> {
    let providers: HashMap<String, RawProvider> = serde_json::from_str(json)?;
    Ok(Catalog { providers })
}

const PER_MILLION: f64 = 1_000_000.0 / 1_000.0; // = 1000: per-1M ÷ 1000 = per-1k

impl Catalog {
    /// Resolve `(provider, model)` to per-1k pricing. Matching order:
    /// exact → case-insensitive model → provider-namespaced id
    /// (`vendor/model`) resolved against the embedded vendor + model.
    pub fn lookup(&self, provider: &str, model: &str) -> Option<PriceInfo> {
        if let Some(p) = self.lookup_in(provider, model) {
            return Some(p);
        }
        // Namespaced id like "anthropic/claude-opus-4-8" (OpenRouter style).
        let (vendor, sub) = model.split_once('/')?;
        self.lookup_in(vendor, sub)
    }

    /// Does the catalog list this `(provider, model)` at all?
    ///
    /// Distinct from `lookup(..).is_some()`, which is also `None` for a model
    /// the catalog knows but prices incompletely. The doctor needs the
    /// difference: "the provider has never heard of this id" is a renamed or
    /// retired model, while "listed but unpriced" is only a display gap.
    pub fn knows(&self, provider: &str, model: &str) -> bool {
        if self.raw_of(provider, model).is_some() {
            return true;
        }
        match model.split_once('/') {
            Some((vendor, sub)) => self.raw_of(vendor, sub).is_some(),
            None => false,
        }
    }

    fn raw_of(&self, provider: &str, model: &str) -> Option<&RawModel> {
        let prov = self.providers.get(provider)?;
        prov.models.get(model).or_else(|| {
            prov.models
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(model))
                .map(|(_, v)| v)
        })
    }

    fn lookup_in(&self, provider: &str, model: &str) -> Option<PriceInfo> {
        let raw = self.raw_of(provider, model)?;
        let cost = raw.cost.as_ref()?;
        Some(PriceInfo {
            input_per_1k: cost.input? / PER_MILLION,
            output_per_1k: cost.output? / PER_MILLION,
            context_window: raw.limit.as_ref().and_then(|l| l.context),
        })
    }

    /// All model ids the catalog lists under `provider`, sorted.
    /// `None` when the catalog has no such provider.
    pub fn provider_models(&self, provider: &str) -> Option<Vec<String>> {
        let prov = self.providers.get(provider)?;
        let mut ids: Vec<String> = prov.models.keys().cloned().collect();
        ids.sort();
        Some(ids)
    }
}

/// Return the path where the catalog cache should be stored.
pub fn cache_path(mur_home: &Path) -> PathBuf {
    mur_home.join("cache").join(CACHE_FILE)
}

/// Load and parse the cached catalog if it exists and is fresh (within ttl_hours).
/// Returns None if the cache file is missing, stale, or unparseable.
pub fn load_cached(mur_home: &Path, ttl_hours: u64) -> Option<Catalog> {
    let path = cache_path(mur_home);
    let meta = std::fs::metadata(&path).ok()?;
    let age = meta.modified().ok()?.elapsed().ok()?;
    if age > std::time::Duration::from_secs(ttl_hours.saturating_mul(3600)) {
        return None;
    }
    let body = std::fs::read_to_string(&path).ok()?;
    parse_catalog(&body).ok()
}

/// Best-effort catalog handle: fresh cache → network fetch (+cache) → stale
/// cache. Unlike [`lookup`], the ladder stops at the first *catalog*, not the
/// first hit — a fresh cache that lacks one model does not trigger a refetch.
pub fn load_or_fetch(mur_home: &Path) -> Option<Catalog> {
    load_cached(mur_home, TTL_HOURS)
        .or_else(|| fetch_and_cache(mur_home))
        .or_else(|| load_cached(mur_home, u64::MAX))
}

/// Fetch the catalog from the network and write it to the cache.
/// Returns the parsed catalog on success; never panics, returns None on any failure.
fn fetch_and_cache(mur_home: &Path) -> Option<Catalog> {
    // `reqwest::blocking` builds its own runtime and panics if constructed
    // inside an existing Tokio runtime — and the `mur` CLI dispatches every
    // command inside `block_on`. Run the blocking HTTP on a dedicated OS
    // thread so it never sees an ambient runtime, regardless of caller.
    let body = std::thread::spawn(|| -> Option<String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .ok()?;
        client.get(CATALOG_URL).send().ok()?.text().ok()
    })
    .join()
    .ok()
    .flatten()?;
    let cat = parse_catalog(&body).ok()?;
    let path = cache_path(mur_home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    Some(cat)
}

/// Resolve pricing for one model. Local tier short-circuits to zero with no IO.
/// Otherwise: fresh cache → network fetch (+cache) → stale cache.
/// All best-effort; returns None when nothing resolves.
pub fn lookup(
    mur_home: &Path,
    provider: &str,
    model: &str,
    tier_is_local: bool,
) -> Option<PriceInfo> {
    if tier_is_local {
        return Some(PriceInfo {
            input_per_1k: 0.0,
            output_per_1k: 0.0,
            context_window: None,
        });
    }
    // Try fresh cache first
    if let Some(cat) = load_cached(mur_home, TTL_HOURS).and_then(|cat| cat.lookup(provider, model))
    {
        return Some(cat);
    }
    // Try network fetch
    if let Some(cat) = fetch_and_cache(mur_home).and_then(|cat| cat.lookup(provider, model)) {
        return Some(cat);
    }
    // Fall back to stale cache
    load_cached(mur_home, u64::MAX).and_then(|cat| cat.lookup(provider, model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

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
        assert!(
            p.is_some(),
            "namespaced id should fall back to embedded provider/model"
        );
    }

    #[test]
    fn local_tier_short_circuits_without_network() {
        let tmp = TempDir::new().unwrap();
        let p = lookup(tmp.path(), "ollama", "llama3", true).unwrap();
        assert_eq!(
            p,
            PriceInfo {
                input_per_1k: 0.0,
                output_per_1k: 0.0,
                context_window: None
            }
        );
        // Verify no cache file was created
        let cache = cache_path(tmp.path());
        assert!(!cache.exists());
    }

    #[test]
    fn load_cached_returns_fresh_cache() {
        let tmp = TempDir::new().unwrap();
        let cache = cache_path(tmp.path());
        if let Some(parent) = cache.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&cache, FIXTURE).unwrap();

        let cat = load_cached(tmp.path(), TTL_HOURS).unwrap();
        let p = cat.lookup("anthropic", "claude-opus-4-8").unwrap();
        assert!((p.input_per_1k - 0.005).abs() < 1e-12);
    }

    #[test]
    fn load_cached_returns_none_for_stale_or_missing() {
        let tmp = TempDir::new().unwrap();
        // Missing cache
        assert!(load_cached(tmp.path(), TTL_HOURS).is_none());

        // Stale cache (write a very old cache with 0 TTL)
        let cache = cache_path(tmp.path());
        if let Some(parent) = cache.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&cache, FIXTURE).unwrap();
        assert!(load_cached(tmp.path(), 0).is_none());
    }

    /// u64::MAX * 3600 would overflow in debug builds (panic) before the
    /// saturating_mul fix.  Verify the offline stale-cache fallback path:
    /// `load_cached(_, u64::MAX)` must return Some and must NOT panic.
    #[test]
    fn stale_fallback_serves_cache_without_panic() {
        let tmp = TempDir::new().unwrap();
        let cache = cache_path(tmp.path());
        if let Some(parent) = cache.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&cache, FIXTURE).unwrap();
        // u64::MAX ttl means "accept any age" — must not overflow or panic.
        let cat = load_cached(tmp.path(), u64::MAX);
        assert!(cat.is_some(), "stale-cache fallback must return Some");
        let p = cat.unwrap().lookup("anthropic", "claude-opus-4-8").unwrap();
        assert!((p.input_per_1k - 0.005).abs() < 1e-12);
    }
}
