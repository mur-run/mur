//! #940: the observability footer must never render a figure whose blank case
//! is indistinguishable from a real one.

use super::footer_segments;
use crate::cmd::agent::cli::footer::Pricing;

/// A model with no per-token price used to render an em dash in the exact
/// slot where a priced model renders `$0.125 est` — so "we cannot price
/// this" and "this cost nothing" were the same pixel. Two agents side by
/// side in a multiplexer is where it bit.
#[test]
fn an_unpriced_model_says_so_rather_than_going_blank() {
    let unpriced = Pricing {
        in_per_1k: None,
        out_per_1k: None,
        window: Some(32_000),
        billing: None,
    };
    let out = footer_segments(10, 20, 100, 200, 3_762, None, &unpriced, None, 200, false);
    assert!(
        out.contains("no price"),
        "an unpriced model must name the reason, got: {out}"
    );
    assert!(
        !out.contains('\u{2014}'),
        "the bare em dash reads as a cost of zero, got: {out}"
    );
}

/// Control: pricing present still produces a number, so the fix above did
/// not simply replace the cost segment.
#[test]
fn a_priced_model_still_shows_the_estimate() {
    let priced = Pricing {
        in_per_1k: Some(3.0),
        out_per_1k: Some(15.0),
        window: Some(32_000),
        billing: None,
    };
    let out = footer_segments(
        10,
        20,
        1_000,
        1_000,
        3_762,
        Some(18.0),
        &priced,
        None,
        200,
        false,
    );
    assert!(out.contains('$'), "priced model must show a cost: {out}");
    assert!(!out.contains("no price"), "got: {out}");
}

/// A subscription or local model is never charged per token, even when its
/// registry entry still carries a per-1k price. The footer used to multiply
/// that price out and show `$x est` for turns that cost nothing.
#[test]
fn an_unbilled_model_names_its_billing_instead_of_a_price() {
    use mur_common::model::BillingMode;
    for (mode, label) in [
        (BillingMode::Subscription, "subscription"),
        (BillingMode::Local, "local"),
    ] {
        let p = Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: Some(32_000),
            billing: Some(mode),
        };
        let open = footer_segments(10, 20, 1_000, 1_000, 3_762, None, &p, None, 200, false);
        assert!(open.contains(label), "{mode:?}: {open}");
        assert!(!open.contains('$'), "{mode:?} priced: {open}");
        assert!(!open.contains("no price"), "{mode:?}: {open}");
        // With a cap, nothing is spent yet but the cap itself stays.
        let capped = footer_segments(
            10,
            20,
            1_000,
            1_000,
            3_762,
            None,
            &p,
            Some(20.0),
            200,
            false,
        );
        assert!(capped.contains(label), "{mode:?}: {capped}");
        assert!(capped.contains("$0.00 / $20.00"), "{mode:?}: {capped}");
        assert!(!capped.contains("no price"), "{mode:?}: {capped}");
    }
}

/// #2609 review: a fallback onto a subscription model must not hide the
/// metered spend earlier turns already made — the figure and the cap stay,
/// and the label says the current model adds nothing.
#[test]
fn metered_spend_stays_visible_on_an_unbilled_turn() {
    use mur_common::model::BillingMode;
    let p = Pricing {
        in_per_1k: Some(3.0),
        out_per_1k: Some(15.0),
        window: None,
        billing: Some(BillingMode::Subscription),
    };
    let capped = footer_segments(
        0,
        0,
        2_000,
        2_000,
        0,
        Some(18.0),
        &p,
        Some(20.0),
        200,
        false,
    );
    assert!(capped.contains("$18.00 / $20.00"), "{capped}");
    assert!(capped.contains("subscription"), "{capped}");
    let open = footer_segments(0, 0, 2_000, 2_000, 0, Some(18.0), &p, None, 200, false);
    assert!(open.contains("$18.000 est"), "{open}");
    assert!(open.contains("subscription"), "{open}");
}
