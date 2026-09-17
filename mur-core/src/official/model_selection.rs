use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use mur_common::{
    model::{BillingMode, ModelEntry, ModelRegistry},
    muragent::manifest::ModelRequirements,
    route::RouteTier,
};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::HashSet};

pub const MAX_FALLBACKS: usize = 3;
const STALE_PRICE_AFTER: Duration = Duration::days(60);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModelSelectionPolicy {
    CapabilityFirst,
    CostFirst,
    PrivacyFirst,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelSelectionWarning {
    UnverifiedToolCapability { model_ref: String },
    UnknownContextWindow { model_ref: String },
    UnknownPrice { model_ref: String },
    StalePrice { model_ref: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSelectionPlan {
    pub primary: String,
    pub fallbacks: Vec<String>,
    pub warnings: Vec<ModelSelectionWarning>,
}

#[derive(Clone, Copy)]
struct Candidate<'a> {
    model_ref: &'a str,
    entry: &'a ModelEntry,
}

fn is_eligible(entry: &ModelEntry, requirements: &ModelRequirements) -> bool {
    if entry.provider.trim().is_empty() || entry.model.trim().is_empty() {
        return false;
    }
    if !entry.capabilities.is_empty() {
        if requirements.chat && !entry.capabilities.iter().any(|cap| cap == "chat") {
            return false;
        }
        if requirements.tools && !entry.capabilities.iter().any(|cap| cap == "tools") {
            return false;
        }
    }
    match requirements.minimum_context_window {
        Some(minimum) => entry.context_window.is_some_and(|window| window >= minimum),
        None => true,
    }
}

fn capability_cmp(a: Candidate<'_>, b: Candidate<'_>) -> Ordering {
    let tier_rank = |entry: &ModelEntry| match entry.effective_route_tier() {
        RouteTier::Frontier => 0_u8,
        RouteTier::Local => 1,
    };
    tier_rank(a.entry)
        .cmp(&tier_rank(b.entry))
        .then_with(|| option_desc(a.entry.context_window, b.entry.context_window))
        .then_with(|| local_rank(a.entry).cmp(&local_rank(b.entry)))
        .then_with(|| tie_break(a, b))
}

fn cost_cmp(a: Candidate<'_>, b: Candidate<'_>) -> Ordering {
    let billing_rank = |entry: &ModelEntry| match entry.billing_or_inferred() {
        BillingMode::Local => 0_u8,
        BillingMode::Subscription => 1,
        BillingMode::UsageBilled => 2,
    };
    billing_rank(a.entry)
        .cmp(&billing_rank(b.entry))
        .then_with(|| {
            if a.entry.billing_or_inferred() == BillingMode::UsageBilled {
                option_f64_asc(
                    a.entry.projected_cost(4_000, 1_000),
                    b.entry.projected_cost(4_000, 1_000),
                )
            } else {
                Ordering::Equal
            }
        })
        .then_with(|| {
            let tier_rank = |entry: &ModelEntry| match entry.effective_route_tier() {
                RouteTier::Frontier => 0_u8,
                RouteTier::Local => 1,
            };
            tier_rank(a.entry).cmp(&tier_rank(b.entry))
        })
        .then_with(|| option_desc(a.entry.context_window, b.entry.context_window))
        .then_with(|| tie_break(a, b))
}

fn local_rank(entry: &ModelEntry) -> u8 {
    u8::from(entry.billing_or_inferred() != BillingMode::Local)
}

fn option_desc(a: Option<u64>, b: Option<u64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn option_f64_asc(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn tie_break(a: Candidate<'_>, b: Candidate<'_>) -> Ordering {
    let verified_rank = |entry: &ModelEntry| u8::from(entry.catalog_verified != Some(true));
    let tools_rank =
        |entry: &ModelEntry| u8::from(!entry.capabilities.iter().any(|cap| cap == "tools"));
    verified_rank(a.entry)
        .cmp(&verified_rank(b.entry))
        .then_with(|| tools_rank(a.entry).cmp(&tools_rank(b.entry)))
        .then_with(|| a.model_ref.cmp(b.model_ref))
}

fn warnings_for(candidates: &[Candidate<'_>], now: DateTime<Utc>) -> Vec<ModelSelectionWarning> {
    let mut warnings = Vec::new();
    for candidate in candidates {
        let model_ref = candidate.model_ref.to_string();
        if candidate.entry.capabilities.is_empty() {
            warnings.push(ModelSelectionWarning::UnverifiedToolCapability {
                model_ref: model_ref.clone(),
            });
        }
        if candidate.entry.context_window.is_none() {
            warnings.push(ModelSelectionWarning::UnknownContextWindow {
                model_ref: model_ref.clone(),
            });
        }
        if candidate.entry.projected_cost(4_000, 1_000).is_none() {
            warnings.push(ModelSelectionWarning::UnknownPrice {
                model_ref: model_ref.clone(),
            });
        } else if candidate
            .entry
            .price_age(now)
            .is_some_and(|age| age > STALE_PRICE_AFTER)
        {
            warnings.push(ModelSelectionWarning::StalePrice { model_ref });
        }
    }
    warnings
}

fn build_plan(candidates: Vec<Candidate<'_>>, now: DateTime<Utc>) -> Result<ModelSelectionPlan> {
    let (primary, rest) = candidates
        .split_first()
        .context("no eligible models satisfy the package requirements")?;
    let selected: Vec<_> = std::iter::once(*primary)
        .chain(rest.iter().copied().take(MAX_FALLBACKS))
        .collect();
    Ok(ModelSelectionPlan {
        primary: primary.model_ref.to_string(),
        fallbacks: selected
            .iter()
            .skip(1)
            .map(|candidate| candidate.model_ref.to_string())
            .collect(),
        warnings: warnings_for(&selected, now),
    })
}

pub fn plan_model_selection(
    registry: &ModelRegistry,
    requirements: &ModelRequirements,
    policy: ModelSelectionPolicy,
    now: DateTime<Utc>,
) -> Result<ModelSelectionPlan> {
    let mut candidates: Vec<_> = registry
        .models
        .iter()
        .filter(|(_, entry)| is_eligible(entry, requirements))
        .filter(|(_, entry)| {
            policy != ModelSelectionPolicy::PrivacyFirst
                || entry.billing_or_inferred() == BillingMode::Local
        })
        .map(|(model_ref, entry)| Candidate { model_ref, entry })
        .collect();
    candidates.sort_by(|a, b| match policy {
        ModelSelectionPolicy::CapabilityFirst | ModelSelectionPolicy::PrivacyFirst => {
            capability_cmp(*a, *b)
        }
        ModelSelectionPolicy::CostFirst => cost_cmp(*a, *b),
    });
    build_plan(candidates, now)
}

pub fn validate_explicit_selection(
    registry: &ModelRegistry,
    requirements: &ModelRequirements,
    primary: &str,
    fallbacks: &[String],
    now: DateTime<Utc>,
) -> Result<ModelSelectionPlan> {
    let mut refs = Vec::with_capacity(1 + fallbacks.len());
    let mut seen = HashSet::new();
    for model_ref in std::iter::once(primary).chain(fallbacks.iter().map(String::as_str)) {
        if !seen.insert(model_ref) {
            continue;
        }
        let entry = registry
            .models
            .get(model_ref)
            .with_context(|| format!("unknown model ref '{model_ref}'"))?;
        if !is_eligible(entry, requirements) {
            bail!("model ref '{model_ref}' does not satisfy the package requirements");
        }
        refs.push(Candidate { model_ref, entry });
    }
    if refs.len().saturating_sub(1) > MAX_FALLBACKS {
        bail!("at most {MAX_FALLBACKS} fallback models are allowed");
    }
    build_plan(refs, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{
        model::{BillingMode, ModelEntry},
        route::RouteTier,
    };

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 17, 12, 0, 0).unwrap()
    }

    fn requirements() -> ModelRequirements {
        ModelRequirements {
            chat: true,
            tools: true,
            minimum_context_window: None,
        }
    }

    fn entry(provider: &str, model: &str) -> ModelEntry {
        ModelEntry {
            provider: provider.into(),
            model: model.into(),
            capabilities: vec!["chat".into(), "tools".into()],
            ..Default::default()
        }
    }

    fn registry(entries: impl IntoIterator<Item = (&'static str, ModelEntry)>) -> ModelRegistry {
        let mut registry = ModelRegistry::default();
        for (key, value) in entries {
            registry.models.insert(key.into(), value);
        }
        registry
    }

    fn plan(registry: &ModelRegistry, policy: ModelSelectionPolicy) -> ModelSelectionPlan {
        plan_model_selection(registry, &requirements(), policy, now()).unwrap()
    }

    #[test]
    fn model_selection_capability_first_orders_tier_context_and_local_ties() {
        let mut frontier_small = entry("openai", "frontier-small");
        frontier_small.tier = Some(RouteTier::Frontier);
        frontier_small.context_window = Some(16_000);
        let mut frontier_large = entry("openai", "frontier-large");
        frontier_large.tier = Some(RouteTier::Frontier);
        frontier_large.context_window = Some(128_000);
        let mut equivalent_local = entry("ollama", "local-frontier");
        equivalent_local.tier = Some(RouteTier::Frontier);
        equivalent_local.context_window = Some(128_000);
        let mut local = entry("ollama", "local");
        local.tier = Some(RouteTier::Local);
        local.context_window = Some(1_000_000);
        let registry = registry([
            ("frontier-small", frontier_small),
            ("frontier-large", frontier_large),
            ("equivalent-local", equivalent_local),
            ("local", local),
        ]);

        let result = plan(&registry, ModelSelectionPolicy::CapabilityFirst);
        assert_eq!(result.primary, "equivalent-local");
        assert_eq!(
            result.fallbacks,
            ["frontier-large", "frontier-small", "local"]
        );
    }

    #[test]
    fn model_selection_cost_first_orders_billing_then_projected_cost() {
        let mut local = entry("ollama", "local");
        local.billing = Some(BillingMode::Local);
        let mut subscription = entry("claude", "subscription");
        subscription.billing = Some(BillingMode::Subscription);
        let mut known_high = entry("openai", "known-high");
        known_high.billing = Some(BillingMode::UsageBilled);
        known_high.input_cost_per_1k = Some(2.0);
        known_high.output_cost_per_1k = Some(1.0);
        let mut known_low = entry("openai", "known-low");
        known_low.billing = Some(BillingMode::UsageBilled);
        known_low.input_cost_per_1k = Some(0.1);
        known_low.output_cost_per_1k = Some(2.0);
        let mut unknown = entry("openai", "unknown");
        unknown.billing = Some(BillingMode::UsageBilled);
        let registry = registry([
            ("unknown", unknown),
            ("known-high", known_high),
            ("subscription", subscription),
            ("local", local),
            ("known-low", known_low),
        ]);

        let result = plan(&registry, ModelSelectionPolicy::CostFirst);
        assert_eq!(result.primary, "local");
        assert_eq!(
            result.fallbacks,
            ["subscription", "known-low", "known-high"]
        );
        assert!(!result.fallbacks.contains(&"unknown".to_string()));
    }

    #[test]
    fn model_selection_unknown_cost_is_not_free() {
        let mut known = entry("openai", "known");
        known.input_cost_per_1k = Some(100.0);
        known.output_cost_per_1k = Some(100.0);
        let unknown = entry("openai", "unknown");
        let registry = registry([("unknown", unknown), ("known", known)]);
        let result = plan(&registry, ModelSelectionPolicy::CostFirst);
        assert_eq!(result.primary, "known");
        assert_eq!(result.fallbacks, ["unknown"]);
    }

    #[test]
    fn model_selection_privacy_first_excludes_non_local_models() {
        let local = entry("ollama", "local");
        let cloud = entry("openai", "cloud");
        let registry = registry([("cloud", cloud), ("local", local)]);
        let result = plan(&registry, ModelSelectionPolicy::PrivacyFirst);
        assert_eq!(result.primary, "local");
        assert!(result.fallbacks.is_empty());
    }

    #[test]
    fn model_selection_enforces_capabilities_identity_and_hard_context() {
        let mut no_tools = entry("openai", "no-tools");
        no_tools.capabilities = vec!["chat".into()];
        let mut legacy = entry("openai", "legacy");
        legacy.capabilities.clear();
        legacy.context_window = Some(64_000);
        let mut unknown_context = entry("openai", "unknown-context");
        unknown_context.context_window = None;
        let mut too_small = entry("openai", "too-small");
        too_small.context_window = Some(8_000);
        let mut empty_provider = entry("", "empty-provider");
        empty_provider.context_window = Some(64_000);
        let mut eligible = entry("openai", "eligible");
        eligible.context_window = Some(64_000);
        let registry = registry([
            ("no-tools", no_tools),
            ("legacy", legacy),
            ("unknown-context", unknown_context),
            ("too-small", too_small),
            ("empty-provider", empty_provider),
            ("eligible", eligible),
        ]);
        let requirements = ModelRequirements {
            minimum_context_window: Some(32_000),
            ..requirements()
        };

        let result = plan_model_selection(
            &registry,
            &requirements,
            ModelSelectionPolicy::CapabilityFirst,
            now(),
        )
        .unwrap();
        assert_eq!(result.primary, "eligible");
        assert_eq!(result.fallbacks, ["legacy"]);
        assert!(
            result
                .warnings
                .contains(&ModelSelectionWarning::UnverifiedToolCapability {
                    model_ref: "legacy".into()
                })
        );
    }

    #[test]
    fn model_selection_warns_for_unknown_context_price_and_stale_price() {
        let mut legacy = entry("openai", "legacy");
        legacy.capabilities.clear();
        let mut stale = entry("openai", "stale");
        stale.input_cost_per_1k = Some(1.0);
        stale.output_cost_per_1k = Some(1.0);
        stale.priced_at = Some(now() - chrono::Duration::days(61));
        let registry = registry([("legacy", legacy), ("stale", stale)]);

        let result = plan(&registry, ModelSelectionPolicy::CapabilityFirst);
        for warning in [
            ModelSelectionWarning::UnverifiedToolCapability {
                model_ref: "legacy".into(),
            },
            ModelSelectionWarning::UnknownContextWindow {
                model_ref: "legacy".into(),
            },
            ModelSelectionWarning::UnknownPrice {
                model_ref: "legacy".into(),
            },
            ModelSelectionWarning::StalePrice {
                model_ref: "stale".into(),
            },
        ] {
            assert!(result.warnings.contains(&warning), "missing {warning:?}");
        }
    }

    #[test]
    fn model_selection_ties_use_verification_tools_then_key_order() {
        let mut unverified = entry("openai", "unverified");
        unverified.catalog_verified = Some(false);
        let mut verified_legacy = entry("openai", "verified-legacy");
        verified_legacy.catalog_verified = Some(true);
        verified_legacy.capabilities.clear();
        let mut verified_tools_z = entry("openai", "verified-tools-z");
        verified_tools_z.catalog_verified = Some(true);
        let mut verified_tools_a = entry("openai", "verified-tools-a");
        verified_tools_a.catalog_verified = Some(true);
        let registry = registry([
            ("z-tools", verified_tools_z),
            ("a-tools", verified_tools_a),
            ("verified-legacy", verified_legacy),
            ("unverified", unverified),
        ]);

        let result = plan(&registry, ModelSelectionPolicy::CapabilityFirst);
        assert_eq!(result.primary, "a-tools");
        assert_eq!(
            result.fallbacks,
            ["z-tools", "verified-legacy", "unverified"]
        );
    }

    #[test]
    fn model_selection_chain_is_unique_and_bounded() {
        let registry = registry([
            ("a", entry("openai", "a")),
            ("b", entry("openai", "b")),
            ("c", entry("openai", "c")),
            ("d", entry("openai", "d")),
            ("e", entry("openai", "e")),
        ]);
        let result = plan(&registry, ModelSelectionPolicy::CapabilityFirst);
        assert_eq!(result.primary, "a");
        assert_eq!(result.fallbacks, ["b", "c", "d"]);
        assert_eq!(result.fallbacks.len(), MAX_FALLBACKS);
        assert!(!result.fallbacks.contains(&result.primary));
    }

    #[test]
    fn model_selection_explicit_selection_validates_deduplicates_and_warns() {
        let mut legacy = entry("openai", "legacy");
        legacy.capabilities.clear();
        let registry = registry([("primary", entry("openai", "primary")), ("legacy", legacy)]);
        let result = validate_explicit_selection(
            &registry,
            &requirements(),
            "primary",
            &["legacy".into(), "primary".into(), "legacy".into()],
            now(),
        )
        .unwrap();
        assert_eq!(result.primary, "primary");
        assert_eq!(result.fallbacks, ["legacy"]);
        assert!(
            validate_explicit_selection(&registry, &requirements(), "missing", &[], now()).is_err()
        );
    }

    #[test]
    fn model_selection_no_candidate_is_an_error() {
        let registry = registry([("cloud", entry("openai", "cloud"))]);
        let error = plan_model_selection(
            &registry,
            &requirements(),
            ModelSelectionPolicy::PrivacyFirst,
            now(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("no eligible"));
    }
}
