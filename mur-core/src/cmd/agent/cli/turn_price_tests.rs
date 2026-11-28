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
        plain_footer(&configured, &b, &usage),
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
        plain_footer(&configured, &b, &usage),
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
        plain_footer(&configured, &b, &usage),
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
        plain_footer(&configured, &b, &usage),
        "  2000 tok · <=$60.000 upper estimate, no rate for mystery-model"
    );
}

#[test]
fn without_model_ref_the_configured_model_prices_the_turn() {
    let b = book();
    let configured = b.pricing_for_key("metered");
    let usage = serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000 });
    assert_eq!(
        plain_footer(&configured, &b, &usage),
        "  2000 tok · $18.000"
    );
}

#[test]
fn a_metered_answering_model_without_rates_is_counted_at_the_dearest_rates() {
    let mut b = book();
    b.by_key.insert(
        "norate".to_string(),
        Pricing {
            in_per_1k: None,
            out_per_1k: None,
            window: None,
            billing: Some(BillingMode::UsageBilled),
        },
    );
    b.key_of_model
        .insert("norate-model".to_string(), "norate".to_string());
    let configured = b.pricing_for_key("metered");
    let legacy = serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "norate-model",
    });
    // Before: "  2000 tok" — a legacy runtime's turn added nothing to the cap.
    assert_eq!(
        plain_footer(&configured, &b, &legacy),
        "  2000 tok · <=$18.000 upper estimate, no rate for norate-model (key norate)"
    );
    // The same turn from a runtime that reports `by_model` costs the same.
    let split = serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "norate-model",
        "by_model": [{ "model": "norate-model", "input_tokens": 1000, "output_tokens": 1000 }],
    });
    assert_eq!(
        plain_footer(&configured, &b, &split),
        "  2000 tok · <=$18.000 upper estimate, no rate for norate-model (key norate)"
    );
}

/// #2620: a turn priced at the dearest-rate fallback is flagged as an upper
/// estimate, the same on a legacy runtime (no `by_model`) and one that sends
/// the split; a fully priced turn on either is not flagged.
#[test]
fn only_a_fallback_priced_turn_is_flagged_on_either_runtime() {
    let b = book();
    let legacy = serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "mystery-model",
    });
    let split = serde_json::json!({
        "input_tokens": 2000, "output_tokens": 2000, "model_ref": "metered-model",
        "by_model": [
            { "model": "mystery-model", "input_tokens": 1000, "output_tokens": 1000 },
            { "model": "metered-model", "registry_key": "metered",
              "input_tokens": 1000, "output_tokens": 1000 },
        ],
    });
    let u = |v: &serde_json::Value| footer::parse_usage(v);
    let configured = b.pricing_for_key("metered");
    let p = price_turn(&configured, Some(&b), &legacy, &u(&legacy));
    assert_eq!(
        (p.usd, p.guessed),
        (Some(18.0), vec!["mystery-model".to_string()])
    );
    let p = price_turn(&configured, Some(&b), &split, &u(&split));
    assert_eq!(
        (p.usd, p.guessed),
        (Some(36.0), vec!["mystery-model".to_string()])
    );

    let priced_legacy = serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "metered-model",
    });
    let priced_split = serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "metered-model",
        "by_model": [{ "model": "metered-model", "registry_key": "metered",
                       "input_tokens": 1000, "output_tokens": 1000 }],
    });
    for usage in [&priced_legacy, &priced_split] {
        assert_eq!(plain_footer(&configured, &b, usage), "  2000 tok · $18.000");
    }
}

/// `book()` plus `norate`: metered, known to the registry, no unit price.
fn book_with_norate() -> PricingBook {
    let mut b = book();
    b.by_key.insert(
        "norate".to_string(),
        Pricing {
            in_per_1k: None,
            out_per_1k: None,
            window: None,
            billing: Some(BillingMode::UsageBilled),
        },
    );
    b.key_of_model
        .insert("norate-model".to_string(), "norate".to_string());
    b
}

/// qa review on #2623: every arm that falls back to the dearest rates must
/// also flag the turn, so flipping any one of them to "not guessed" fails here.
#[test]
fn every_dearest_rate_arm_flags_the_turn_and_names_the_model() {
    let b = book_with_norate();
    let u = |v: &serde_json::Value| footer::parse_usage(v);
    let case = |pricing: &Pricing, usage: serde_json::Value| {
        let p = price_turn(pricing, Some(&b), &usage, &u(&usage));
        (p.usd, p.guessed)
    };
    let one = |s: &str| (Some(18.0), vec![s.to_string()]);

    // Legacy runtime, no `model_ref`: the configured metered model has no rate.
    let mut nb = book_with_norate();
    nb.configured_key = Some("norate".to_string());
    let usage = serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000 });
    let p = price_turn(&nb.pricing_for_key("norate"), Some(&nb), &usage, &u(&usage));
    assert_eq!((p.usd, p.guessed), one("norate"));

    // A share whose `registry_key` resolves to an unpriced entry: the notice
    // names the provider id AND the key `mur model add` takes.
    let configured = b.pricing_for_key("metered");
    assert_eq!(
        case(
            &configured,
            serde_json::json!({
                "input_tokens": 1000, "output_tokens": 1000,
                "by_model": [{ "model": "vendor/norate-v2", "registry_key": "norate",
                               "input_tokens": 1000, "output_tokens": 1000 }],
            })
        ),
        one("vendor/norate-v2 (key norate)")
    );

    // A share naming neither a model nor a registry key.
    assert_eq!(
        case(
            &configured,
            serde_json::json!({
                "input_tokens": 1000, "output_tokens": 1000,
                "by_model": [{ "input_tokens": 1000, "output_tokens": 1000 }],
            })
        ),
        one(UNNAMED_MODEL)
    );

    // Several guessed shares: sorted, deduplicated, all counted.
    assert_eq!(
        case(
            &configured,
            serde_json::json!({
                "input_tokens": 3000, "output_tokens": 3000,
                "by_model": [
                    { "model": "zeta-model", "input_tokens": 1000, "output_tokens": 1000 },
                    { "model": "alpha-model", "input_tokens": 1000, "output_tokens": 1000 },
                    { "model": "zeta-model", "input_tokens": 1000, "output_tokens": 1000 },
                ],
            })
        ),
        (
            Some(54.0),
            vec!["alpha-model".to_string(), "zeta-model".to_string()]
        )
    );
}
