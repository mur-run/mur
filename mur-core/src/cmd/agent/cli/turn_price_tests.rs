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
        "  2000 tok · ≤$60.000 upper estimate, no rate for mystery-model"
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
        "  2000 tok · ≤$18.000 upper estimate, no rate for norate-model"
    );
    // The same turn from a runtime that reports `by_model` costs the same.
    let split = serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "norate-model",
        "by_model": [{ "model": "norate-model", "input_tokens": 1000, "output_tokens": 1000 }],
    });
    assert_eq!(
        plain_footer(&configured, &b, &split),
        "  2000 tok · ≤$18.000 upper estimate, no rate for norate-model"
    );
}

/// #2609 review: every `by_model` share answered by a subscription model while
/// the agent is configured on a metered one. Nothing was billed per token, so
/// the line names the billing mode — not `$0.000`, not the configured rates.
#[test]
fn an_all_subscription_turn_on_a_metered_config_is_labelled_subscription() {
    let b = book();
    let configured = b.pricing_for_key("metered");
    let usage = serde_json::json!({
        "input_tokens": 2000, "output_tokens": 2000, "model_ref": "sub-model",
        "by_model": [
            { "model": "sub-model", "registry_key": "sub",
              "input_tokens": 1000, "output_tokens": 1000 },
            { "model": "sub-model", "registry_key": "sub",
              "input_tokens": 1000, "output_tokens": 1000 },
        ],
    });
    assert_eq!(
        plain_footer(&configured, &b, &usage),
        "  4000 tok · subscription"
    );
}

/// #2609 review: `--plain` said nothing when the runtime fell back, while the
/// TUI printed `⇄ answered by '…'`. Both now share one once-per-change rule.
#[test]
fn plain_mode_announces_a_fallback_once_per_change() {
    let b = book();
    let turn =
        |m: &str| serde_json::json!({ "input_tokens": 1, "output_tokens": 1, "model_ref": m });
    let mut last = None;
    // The configured model answering is not a fallback.
    assert_eq!(
        plain_fallback_notice(&b, &turn("metered-model"), &mut last),
        None
    );
    let first = plain_fallback_notice(&b, &turn("sub-model"), &mut last).expect("notice");
    assert!(
        first.starts_with("⇄ answered by 'sub' (sub-model), not 'metered'"),
        "{first}"
    );
    assert!(first.contains("subscription"), "{first}");
    // Same substitute again: said once already.
    assert_eq!(
        plain_fallback_notice(&b, &turn("sub-model"), &mut last),
        None
    );
    // No model_ref, or one the registry does not know: nothing to name.
    let bare = serde_json::json!({ "input_tokens": 1, "output_tokens": 1 });
    assert_eq!(plain_fallback_notice(&b, &bare, &mut last), None);
    assert_eq!(
        plain_fallback_notice(&b, &turn("mystery-model"), &mut last),
        None
    );
}

/// #2625 review (Major): a profile that names no model has nothing to fall
/// back *from*, so whichever model answers is not announced as a fallback.
#[test]
fn no_configured_model_never_announces_a_fallback() {
    let mut b = book();
    b.configured_key = None;
    let turn =
        |m: &str| serde_json::json!({ "input_tokens": 1, "output_tokens": 1, "model_ref": m });
    let mut last = None;
    for m in ["metered-model", "sub-model", "metered-model"] {
        assert_eq!(plain_fallback_notice(&b, &turn(m), &mut last), None, "{m}");
    }
}

/// #2625 review: every switch away from the configured model is announced —
/// configured → fallback → configured → fallback says so twice, and an
/// unresolvable model in between breaks the run (sub → unknown → sub).
#[test]
fn each_return_to_a_fallback_is_announced_again() {
    let b = book();
    let turn =
        |m: &str| serde_json::json!({ "input_tokens": 1, "output_tokens": 1, "model_ref": m });
    let mut last = None;
    let mut says = |m: &str| plain_fallback_notice(&b, &turn(m), &mut last).is_some();
    assert!(!says("metered-model"));
    assert!(says("sub-model"));
    assert!(!says("metered-model"));
    assert!(says("sub-model"));
    assert!(!says("mystery-model"));
    assert!(says("sub-model"));
}

/// #2625 review I2: the notice follows the turn's last call (`model_ref`),
/// as the TUI does. A fallback that answered only mid-turn is not announced,
/// while its share is still priced at its own rates from `by_model`.
#[test]
fn a_mid_turn_fallback_is_not_announced_when_the_last_call_is_configured() {
    let b = book();
    let usage = serde_json::json!({
        "input_tokens": 2000, "output_tokens": 2000, "model_ref": "metered-model",
        "by_model": [
            { "model": "sub-model", "registry_key": "sub", "input_tokens": 1000, "output_tokens": 1000 },
            { "model": "metered-model", "registry_key": "metered", "input_tokens": 1000, "output_tokens": 1000 }
        ]
    });
    let mut last = None;
    assert_eq!(plain_fallback_notice(&b, &usage, &mut last), None);
    assert_eq!(last.as_deref(), Some("metered"));
    // The next turn ending on the fallback is still announced.
    let next =
        serde_json::json!({ "input_tokens": 1, "output_tokens": 1, "model_ref": "sub-model" });
    assert!(plain_fallback_notice(&b, &next, &mut last).is_some());
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

/// A turn whose runtime sent no `model_ref` says nothing and leaves the run
/// as it was: it neither announces nor resets, so the same fallback after it
/// is not announced a second time.
#[test]
fn a_turn_without_model_ref_neither_announces_nor_resets() {
    let b = book();
    let mut last = None;
    let sub =
        serde_json::json!({ "input_tokens": 1, "output_tokens": 1, "model_ref": "sub-model" });
    let bare = serde_json::json!({ "input_tokens": 1, "output_tokens": 1 });
    assert!(plain_fallback_notice(&b, &sub, &mut last).is_some());
    assert_eq!(plain_fallback_notice(&b, &bare, &mut last), None);
    assert_eq!(last.as_deref(), Some("sub"));
    assert_eq!(plain_fallback_notice(&b, &sub, &mut last), None);
}
