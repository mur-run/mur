//! The `--plain` footer must price and label a turn exactly as the TUI does
//! (qa review I3/I4 on #2609).

use super::*;
use mur_common::model::BillingMode;
use std::collections::HashMap;

fn priced(billing: BillingMode) -> Pricing {
    Pricing {
        in_per_1k: Some(3.0),
        out_per_1k: Some(15.0),
        window: None,
        billing: Some(billing),
    }
}

/// `metered` ($3/$15 per 1k, `metered-model`) and `sub` (subscription,
/// `sub-model`), configured on `metered`.
fn book() -> PricingBook {
    let mut by_key = HashMap::new();
    let mut key_of_model = HashMap::new();
    by_key.insert("metered".to_string(), priced(BillingMode::UsageBilled));
    key_of_model.insert("metered-model".to_string(), "metered".to_string());
    by_key.insert("sub".to_string(), priced(BillingMode::Subscription));
    key_of_model.insert("sub-model".to_string(), "sub".to_string());
    PricingBook {
        by_key,
        key_of_model,
        configured_key: Some("metered".to_string()),
    }
}

#[test]
fn a_subscription_fallback_turn_is_labelled_subscription() {
    let b = book();
    let configured = b.pricing_for_key("metered");
    let usage = serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "sub-model",
    });
    // Before: priced at the configured metered model → "$18.000".
    assert_eq!(
        plain_footer(&configured, Some(&b), &usage),
        "  2000 tok · subscription"
    );
}

#[test]
fn a_metered_fallback_from_a_subscription_config_shows_its_cost() {
    let b = book();
    let configured = b.pricing_for_key("sub");
    let usage = serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "metered-model",
    });
    // Before: labelled "subscription" while the turn was billed.
    assert_eq!(
        plain_footer(&configured, Some(&b), &usage),
        "  2000 tok · $18.000"
    );
}

#[test]
fn a_mixed_turn_shows_only_its_metered_share() {
    let b = book();
    let configured = b.pricing_for_key("metered");
    let usage = serde_json::json!({
        "input_tokens": 2000, "output_tokens": 2000, "model_ref": "sub-model",
        "by_model": [
            { "model": "metered-model", "registry_key": "metered",
              "input_tokens": 1000, "output_tokens": 1000 },
            { "model": "sub-model", "registry_key": "sub",
              "input_tokens": 1000, "output_tokens": 1000 },
        ],
    });
    assert_eq!(
        plain_footer(&configured, Some(&b), &usage),
        "  4000 tok · $18.000"
    );
}

/// An older runtime sends no `by_model`; an answering model the registry
/// cannot resolve is then counted at the book's DEAREST rates, the same
/// fail-closed rule as an unresolvable share — never at the configured
/// model's rates, and never as free.
#[test]
fn an_unknown_answering_model_without_shares_is_counted_at_the_dearest_rates() {
    let mut b = book();
    b.by_key.insert(
        "premium".to_string(),
        Pricing {
            in_per_1k: Some(10.0),
            out_per_1k: Some(50.0),
            window: None,
            billing: Some(BillingMode::UsageBilled),
        },
    );
    let configured = b.pricing_for_key("metered");
    let usage = serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "mystery-model",
    });
    // Before: "  2000 tok" — the turn silently added nothing to the cap.
    assert_eq!(
        plain_footer(&configured, Some(&b), &usage),
        "  2000 tok · $60.000"
    );
}

#[test]
fn without_model_ref_the_configured_model_prices_the_turn() {
    let b = book();
    let configured = b.pricing_for_key("metered");
    let usage = serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000 });
    assert_eq!(
        plain_footer(&configured, Some(&b), &usage),
        "  2000 tok · $18.000"
    );
}
