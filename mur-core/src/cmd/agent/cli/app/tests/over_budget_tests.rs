//! `over_budget_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;

#[test]
fn over_budget_only_when_priced_and_at_or_past_cap() {
    let mut a = App::test_fixture();
    a.pricing = super::super::super::footer::Pricing {
        in_per_1k: Some(3.0),
        out_per_1k: Some(15.0),
        window: None,
        billing: None,
    };
    a.apply_usage(&serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000 })); // $18.00
    a.budget_usd = None;
    assert!(!a.over_budget()); // no cap
    a.budget_usd = Some(20.0);
    assert!(!a.over_budget()); // under
    a.budget_usd = Some(18.0);
    assert!(a.over_budget()); // at cap
    a.budget_usd = Some(5.0);
    assert!(a.over_budget()); // over
    let mut b = App::test_fixture(); // unpriced → fail OPEN
    b.apply_usage(&serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000 }));
    b.budget_usd = Some(0.01);
    assert!(!b.over_budget());
}

/// A subscription or local model never spends against the cap, even with a
/// stale per-1k price on its registry entry — it used to lock the session out
/// of turns that cost nothing.
#[test]
fn an_unbilled_model_never_trips_the_cap() {
    use mur_common::model::BillingMode;
    let turn = serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000 });
    let run = |billing| {
        let mut a = App::test_fixture();
        a.budget_usd = Some(0.01);
        a.pricing = super::super::super::footer::Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: None,
            billing: Some(billing),
        };
        a.apply_usage(&turn);
        a
    };
    for mode in [BillingMode::Subscription, BillingMode::Local] {
        let a = run(mode);
        assert!(!a.over_budget(), "{mode:?} must not count against the cap");
        assert_eq!(a.session_cost(), None, "{mode:?}");
    }
    // Control: a metered model at the same rates still trips it.
    assert!(run(BillingMode::UsageBilled).over_budget());
}

/// A registry with one metered and one subscription model, so turns can be
/// answered by either through the runtime's real `usage.model_ref` path.
fn mixed_book_app() -> App {
    use super::super::super::PricingBook;
    use super::super::super::footer::Pricing;
    use mur_common::model::BillingMode;
    use std::collections::HashMap;
    let mut by_key = HashMap::new();
    let mut key_of_model = HashMap::new();
    by_key.insert(
        "metered".to_string(),
        Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: None,
            billing: Some(BillingMode::UsageBilled),
        },
    );
    key_of_model.insert("metered-model".to_string(), "metered".to_string());
    by_key.insert(
        "sub".to_string(),
        Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: None,
            billing: Some(BillingMode::Subscription),
        },
    );
    key_of_model.insert("sub-model".to_string(), "sub".to_string());
    let mut a = App::test_fixture();
    a.pricing = by_key["metered"];
    a.pricing_book = Some(PricingBook {
        by_key,
        key_of_model,
        configured_key: Some("metered".to_string()),
    });
    a
}

fn answered(model: &str, input: u64, output: u64) -> serde_json::Value {
    serde_json::json!({ "input_tokens": input, "output_tokens": output, "model_ref": model })
}

/// PR #2609 review: one fallback turn on a subscription model priced the
/// WHOLE session as unbilled, so metered spend already made vanished from
/// the cap and the gate failed open. Spend accrues per turn, at the rates of
/// the model that answered that turn.
#[test]
fn metered_spend_survives_a_subscription_turn() {
    let mut a = mixed_book_app();
    a.budget_usd = Some(20.0);
    a.apply_usage(&answered("metered-model", 1000, 1000)); // 3 + 15 = $18
    assert_eq!(a.session_cost(), Some(18.0));
    a.apply_usage(&answered("sub-model", 1000, 1000));
    assert_eq!(
        a.session_cost(),
        Some(18.0),
        "a subscription turn adds nothing and erases nothing"
    );
    assert!(!a.over_budget());
    a.apply_usage(&answered("metered-model", 1000, 0)); // + $3
    assert_eq!(
        a.session_cost(),
        Some(21.0),
        "the subscription turn's tokens are not repriced at metered rates"
    );
    assert!(a.over_budget());
}

/// A session that only ever ran on unbilled models has no money figure at
/// all, so the footer can name who pays instead.
#[test]
fn an_all_subscription_session_has_no_cost() {
    let mut a = mixed_book_app();
    a.budget_usd = Some(0.01);
    a.apply_usage(&answered("sub-model", 1000, 1000));
    a.apply_usage(&answered("sub-model", 1000, 1000));
    assert_eq!(a.session_cost(), None);
    assert!(!a.over_budget());
}

fn turn_with_shares(last: &str, shares: serde_json::Value) -> serde_json::Value {
    let (i, o) = shares.as_array().unwrap().iter().fold((0, 0), |(i, o), s| {
        (
            i + s["input_tokens"].as_u64().unwrap(),
            o + s["output_tokens"].as_u64().unwrap(),
        )
    });
    serde_json::json!({
        "input_tokens": i, "output_tokens": o, "model_ref": last, "by_model": shares,
    })
}

/// qa review C1: a turn that ran metered calls and then fell back to a
/// subscription model reports the subscription model as `model_ref`. Pricing
/// the turn's totals at that last model counted the metered share as $0.
#[test]
fn a_turn_ending_on_subscription_still_counts_its_metered_share() {
    let mut a = mixed_book_app();
    a.budget_usd = Some(20.0);
    a.apply_usage(&turn_with_shares(
        "sub-model",
        serde_json::json!([
            { "registry_key": "metered", "model": "metered-model", "input_tokens": 1000, "output_tokens": 1000 },
            { "registry_key": "sub", "model": "sub-model", "input_tokens": 5000, "output_tokens": 5000 },
        ]),
    ));
    assert_eq!(a.session_cost(), Some(18.0));
}

/// qa review I1, the mirror case: a turn ending on a metered model must not
/// bill its subscription share at metered rates.
#[test]
fn a_turn_ending_on_metered_does_not_bill_its_subscription_share() {
    let mut a = mixed_book_app();
    a.apply_usage(&turn_with_shares(
        "metered-model",
        serde_json::json!([
            { "registry_key": "sub", "model": "sub-model", "input_tokens": 5000, "output_tokens": 5000 },
            { "registry_key": "metered", "model": "metered-model", "input_tokens": 1000, "output_tokens": 0 },
        ]),
    ));
    assert_eq!(a.session_cost(), Some(3.0));
}

/// qa review I2: a share answered by a model the registry does not know is
/// counted at the registry's dearest metered rates, not dropped — the cap
/// fails closed on what it cannot price, like the fleet guard.
#[test]
fn an_unregistered_share_is_counted_conservatively() {
    let mut a = mixed_book_app();
    a.apply_usage(&turn_with_shares(
        "mystery-model",
        serde_json::json!([
            { "registry_key": null, "model": "mystery-model", "input_tokens": 1000, "output_tokens": 1000 },
        ]),
    ));
    assert_eq!(a.session_cost(), Some(18.0));
}

/// A share naming only a provider model (no registry key) resolves through
/// the same longest-prefix lookup `model_ref` uses.
#[test]
fn a_share_without_registry_key_resolves_by_model_name() {
    let mut a = mixed_book_app();
    a.apply_usage(&turn_with_shares(
        "sub-model-20260101",
        serde_json::json!([
            { "registry_key": null, "model": "metered-model-20260101", "input_tokens": 1000, "output_tokens": 0 },
            { "registry_key": null, "model": "sub-model-20260101", "input_tokens": 1000, "output_tokens": 1000 },
        ]),
    ));
    assert_eq!(a.session_cost(), Some(3.0));
}

/// An older runtime sends no `by_model`. An answering model the registry
/// cannot resolve used to add $0 to the session — "unknown" read as "free",
/// the one thing the cap must not do. It is now counted at the book's dearest
/// rates, the same as an unresolvable share above.
#[test]
fn a_legacy_turn_from_an_unregistered_model_is_counted_conservatively() {
    let mut a = mixed_book_app();
    a.budget_usd = Some(10.0);
    a.apply_usage(&serde_json::json!({
        "input_tokens": 1000, "output_tokens": 1000, "model_ref": "mystery-model",
    }));
    assert_eq!(a.session_cost(), Some(18.0));
    assert!(a.over_budget());
}

/// The fallback notice must not claim the cost is "now priced for" a model
/// that is never charged per token.
#[test]
fn the_fallback_notice_says_what_the_switch_does_to_cost() {
    use crate::cmd::agent::cli::turn_price::fallback_notice;
    let a = mixed_book_app();
    let book = a.pricing_book.as_ref().unwrap();
    assert_eq!(
        fallback_notice("sub", "sub-model", "metered", &book.pricing_for_key("sub")),
        "⇄ answered by 'sub' (sub-model), not 'metered' — the runtime fell back. \
         'sub' is subscription: it adds nothing per token."
    );
    assert_eq!(
        fallback_notice(
            "metered",
            "metered-model",
            "sub",
            &book.pricing_for_key("metered")
        ),
        "⇄ answered by 'metered' (metered-model), not 'sub' — the runtime fell back. \
         Cost below is now priced for 'metered'."
    );
}

/// #2620: a turn counted at the dearest-rate fallback marks the session as an
/// upper estimate and names the model once in the transcript — on a legacy
/// runtime and on one that sends `by_model` alike.
#[test]
fn a_fallback_priced_turn_is_flagged_once_on_either_runtime() {
    let notices = |a: &App| {
        a.messages
            .iter()
            .filter(|m| m.text.contains("no rate in models.yaml"))
            .count()
    };
    let legacy = answered("mystery-model", 1000, 1000);
    let split = turn_with_shares(
        "metered-model",
        serde_json::json!([
            { "registry_key": null, "model": "mystery-model", "input_tokens": 1000, "output_tokens": 1000 },
        ]),
    );
    for usage in [&legacy, &split] {
        let mut a = mixed_book_app();
        a.apply_usage(&answered("metered-model", 1000, 1000));
        assert!(
            a.session_guessed.is_empty(),
            "a priced turn is not an estimate"
        );
        a.apply_usage(usage);
        a.apply_usage(usage);
        assert_eq!(a.session_guessed, vec!["mystery-model".to_string()]);
        assert_eq!(notices(&a), 1, "named once, not every turn");
        let last = a
            .messages
            .iter()
            .rev()
            .find(|m| m.text.contains("no rate"))
            .unwrap();
        assert!(last.text.contains("mystery-model ") && last.text.contains("$3/1k in, $15/1k out"));
    }
}

/// qa nit 3 on #2623: a notice naming several models reads "their tokens".
#[test]
fn a_notice_naming_several_models_uses_the_plural() {
    let mut a = mixed_book_app();
    a.apply_usage(&turn_with_shares(
        "metered-model",
        serde_json::json!([
            { "model": "mystery-a", "input_tokens": 1000, "output_tokens": 1000 },
            { "model": "mystery-b", "input_tokens": 1000, "output_tokens": 1000 },
        ]),
    ));
    let notice = a
        .messages
        .iter()
        .find(|m| m.text.contains("no rate in models.yaml"))
        .expect("a notice");
    assert!(
        notice.text.contains("mystery-a, mystery-b — their tokens"),
        "{}",
        notice.text
    );
}
