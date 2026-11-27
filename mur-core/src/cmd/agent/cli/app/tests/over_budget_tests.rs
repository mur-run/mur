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
