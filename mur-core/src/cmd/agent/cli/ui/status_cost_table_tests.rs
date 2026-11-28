//! The footer's cost segment, every row of its decision table compared as a
//! whole string (qa review on #2609). `contains` checks let a stray separator,
//! a doubled label or a wrong precision through; these do not.

use super::footer_segments;
use crate::cmd::agent::cli::footer::Pricing;
use mur_common::model::BillingMode;

/// Wide enough that every row renders whole; ctx is omitted (no window) and
/// the token pair is fixed, so the row's cost text is the only variable.
const WIDE: usize = 200;
const TOKS: &str = "30/2,000 tok";

fn with(billing: Option<BillingMode>, rates: bool) -> Pricing {
    Pricing {
        in_per_1k: rates.then_some(3.0),
        out_per_1k: rates.then_some(15.0),
        window: None,
        billing,
    }
}

/// (billing, has rates, spent, cap, expected cost text)
type Row = (
    Option<BillingMode>,
    bool,
    Option<f64>,
    Option<f64>,
    &'static str,
);

#[test]
fn every_cost_row_renders_exactly() {
    use BillingMode::{Local, Subscription, UsageBilled};
    let rows: &[Row] = &[
        // Unbilled, nothing metered yet: the label alone without a cap; with
        // one, the cap stays visible — a fallback to a metered model would
        // still count against it.
        (Some(Subscription), true, None, None, "subscription"),
        (
            Some(Subscription),
            true,
            None,
            Some(20.0),
            "$0.00 / $20.00 · subscription",
        ),
        (Some(Local), false, None, None, "local"),
        (
            Some(Local),
            false,
            None,
            Some(20.0),
            "$0.00 / $20.00 · local",
        ),
        // Unbilled after metered spend: the spend stays, the label follows.
        (
            Some(Subscription),
            true,
            Some(18.0),
            Some(20.0),
            "$18.00 / $20.00 · subscription",
        ),
        (
            Some(Subscription),
            true,
            Some(18.0),
            None,
            "$18.000 est · subscription",
        ),
        (
            Some(Local),
            false,
            Some(1.5),
            Some(20.0),
            "$1.50 / $20.00 · local",
        ),
        (Some(Local), false, Some(1.5), None, "$1.500 est · local"),
        // Metered, explicit or unknown billing: money only.
        (
            Some(UsageBilled),
            true,
            Some(18.0),
            Some(20.0),
            "$18.00 / $20.00",
        ),
        (Some(UsageBilled), true, Some(18.0), None, "$18.000 est"),
        (None, true, Some(0.018), None, "$0.018 est"),
        (None, true, Some(18.0), Some(20.0), "$18.00 / $20.00"),
        // Nothing spent yet: priced reads "$0.00 / cap"; unpriced keeps
        // "no price" in the spent slot rather than claiming "$0.00".
        (Some(UsageBilled), true, None, Some(20.0), "$0.00 / $20.00"),
        (None, true, None, Some(20.0), "$0.00 / $20.00"),
        (
            Some(UsageBilled),
            false,
            None,
            Some(20.0),
            "no price / $20.00",
        ),
        (None, false, None, Some(20.0), "no price / $20.00"),
        (Some(UsageBilled), false, None, None, "no price"),
        (None, false, None, None, "no price"),
    ];
    for &(billing, rates, spent, cap, cost) in rows {
        let p = with(billing, rates);
        let got = footer_segments(10, 20, 1_000, 1_000, 0, spent, &p, cap, WIDE, false);
        assert_eq!(
            got,
            format!("{TOKS} · {cost}"),
            "billing={billing:?} rates={rates} spent={spent:?} cap={cap:?}"
        );
    }
}

/// #2620: a session figure that counted fallback-priced tokens carries the
/// upper-bound mark in every money form; with nothing spent there is no figure
/// to mark.
#[test]
fn a_fallback_priced_figure_is_marked_as_an_upper_bound() {
    use BillingMode::{Subscription, UsageBilled};
    type Case = (Option<BillingMode>, Option<f64>, Option<f64>, &'static str);
    let cases: &[Case] = &[
        (Some(UsageBilled), Some(18.0), None, "≤$18.000 est"),
        (
            Some(UsageBilled),
            Some(18.0),
            Some(20.0),
            "≤$18.00 / $20.00",
        ),
        (
            Some(Subscription),
            Some(18.0),
            None,
            "≤$18.000 est · subscription",
        ),
        (Some(UsageBilled), None, Some(20.0), "$0.00 / $20.00"),
    ];
    for &(billing, spent, cap, want) in cases {
        let p = with(billing, true);
        let got = footer_segments(10, 20, 1_000, 1_000, 0, spent, &p, cap, WIDE, true);
        assert_eq!(
            got,
            format!("{TOKS} · {want}"),
            "{billing:?} {spent:?} {cap:?}"
        );
    }
}

/// One recorded rate. A usage-billed model is priced through `own_rates`, so
/// it reads `$0.00 / cap` before the first turn just as it reads `$X / cap`
/// after it; unknown billing goes through `turn_cost`, which needs both rates
/// (#2630 review).
#[test]
fn a_half_priced_model_follows_the_pricer() {
    use BillingMode::UsageBilled;
    type Case = (Option<BillingMode>, Option<f64>, Option<f64>, &'static str);
    let cases: &[Case] = &[
        (Some(UsageBilled), Some(3.0), None, "$0.00 / $20.00"),
        (Some(UsageBilled), None, Some(15.0), "$0.00 / $20.00"),
        (None, Some(3.0), None, "no price / $20.00"),
        (None, None, Some(15.0), "no price / $20.00"),
    ];
    for &(billing, in_per_1k, out_per_1k, want) in cases {
        let p = Pricing {
            in_per_1k,
            out_per_1k,
            window: None,
            billing,
        };
        let got = footer_segments(10, 20, 1_000, 1_000, 0, None, &p, Some(20.0), WIDE, false);
        assert_eq!(
            got,
            format!("{TOKS} · {want}"),
            "{billing:?} in={in_per_1k:?} out={out_per_1k:?}"
        );
    }
}

/// An unpriced model is the one #2620 prices at the fallback rates, so it is
/// the realistic `guessed` case. With nothing spent there is no figure, so no
/// mark either.
#[test]
fn an_unpriced_guessed_model_with_nothing_spent_carries_no_mark() {
    for billing in [None, Some(BillingMode::UsageBilled)] {
        let p = with(billing, false);
        let got = footer_segments(10, 20, 1_000, 1_000, 0, None, &p, Some(20.0), WIDE, true);
        assert_eq!(got, format!("{TOKS} · no price / $20.00"), "{billing:?}");
    }
}

/// A capped row with nothing spent grew from `/ $20.00` (8 chars) to
/// `$0.00 / $20.00` (14) or `no price / $20.00` (17), so it is trimmed at a
/// wider bar than before. Pin the budgets where each segment drops.
#[test]
fn a_capped_unspent_row_trims_tokens_first_then_cost() {
    type Case = (bool, &'static str, usize);
    // (has rates, cost text, full width = TOKS + " · " + cost)
    let cases: &[Case] = &[
        (true, "$0.00 / $20.00", 29),
        (false, "no price / $20.00", 32),
    ];
    for &(rates, cost, full_width) in cases {
        let p = with(None, rates);
        let at =
            |budget| footer_segments(10, 20, 1_000, 1_000, 0, None, &p, Some(20.0), budget, false);
        let full = format!("{TOKS} · {cost}");
        assert_eq!(full.chars().count(), full_width);
        assert_eq!(at(full_width), full);
        assert_eq!(at(full_width - 1), cost, "tokens drop first");
        assert_eq!(at(cost.chars().count()), cost);
        assert_eq!(
            at(cost.chars().count() - 1),
            "",
            "then the cost; no ctx to keep"
        );
    }
}
