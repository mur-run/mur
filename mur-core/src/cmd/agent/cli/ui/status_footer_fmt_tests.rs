//! Footer formatting cases that predate the cost decision table in
//! `status_cost_table_tests.rs`: substring checks on token, cost and context
//! segments, and the narrow-bar trimming order.

use crate::cmd::agent::cli::footer::Pricing;

/// These cases predate the upper-bound mark (#2620): nothing in them was
/// priced at the fallback rates.
#[allow(clippy::too_many_arguments)]
fn footer_segments(
    turn_in: u64,
    turn_out: u64,
    sess_in: u64,
    sess_out: u64,
    ctx_tokens: u64,
    spent: Option<f64>,
    pricing: &Pricing,
    budget_usd: Option<f64>,
    budget: usize,
) -> String {
    super::footer_segments(
        turn_in, turn_out, sess_in, sess_out, ctx_tokens, spent, pricing, budget_usd, budget, false,
    )
}

/// Enough room that these assertions test the formatting, not the trimming.
const WIDE: usize = 200;

#[test]
fn shows_tokens_and_names_the_reason_when_unpriced() {
    let s = footer_segments(1240, 0, 1240, 0, 0, None, &Pricing::default(), None, WIDE);
    assert!(s.contains("1,240 tok") || s.contains("1240 tok"));
    // Was an em dash, which sat in the same slot as "$0.125 est" and so
    // read as a cost of zero rather than an absent price (#940).
    assert!(s.contains("no price"), "got: {s}");
}

#[test]
fn shows_cost_and_ctx_when_priced() {
    let p = Pricing {
        in_per_1k: Some(0.003),
        out_per_1k: Some(0.015),
        window: Some(100_000),
        billing: None,
    };
    let s = footer_segments(1000, 1000, 1000, 1000, 32_000, Some(0.018), &p, None, WIDE);
    assert!(s.contains("$0.018"));
    assert!(s.contains("32%"));
    assert!(!s.contains(" / $")); // no budget suffix when budget is None
}

#[test]
fn shows_budget_suffix_when_cap_set() {
    let p = Pricing {
        in_per_1k: Some(3.0),
        out_per_1k: Some(15.0),
        window: None,
        billing: None,
    };
    // session 1000/1000 → $18.00 spent, cap $20.00
    let s = footer_segments(1000, 1000, 1000, 1000, 0, Some(18.0), &p, Some(20.0), WIDE);
    assert!(s.contains("$18.00 / $20.00"), "got: {s}");
    // unpriced model → "no price" in the spent slot, cap still shown
    let s2 = footer_segments(
        1000,
        1000,
        1000,
        1000,
        0,
        None,
        &Pricing::default(),
        Some(20.0),
        WIDE,
    );
    assert!(s2.contains("no price / $20.00"), "got: {s2}");
    assert!(!s2.contains("$18.00"));
}

#[test]
fn cost_is_the_session_not_the_turn() {
    let p = Pricing {
        in_per_1k: Some(3.0),
        out_per_1k: Some(15.0),
        window: None,
        billing: None,
    };
    // Mid-stream: the turn's usage hasn't been reported yet. The bar used
    // to price the turn, so it read "$0.000 est" beside a session total.
    let s = footer_segments(0, 0, 1000, 1000, 0, Some(18.0), &p, None, WIDE);
    assert!(s.contains("$18.000 est"), "got: {s}");
}

#[test]
fn narrow_bar_keeps_the_context_gauge_and_drops_the_rest() {
    let p = Pricing {
        in_per_1k: Some(0.003),
        out_per_1k: Some(0.015),
        window: Some(100_000),
        billing: None,
    };
    let full = footer_segments(1000, 1000, 1000, 1000, 91_000, Some(0.018), &p, None, WIDE);
    let tight = footer_segments(1000, 1000, 1000, 1000, 91_000, Some(0.018), &p, None, 20);
    assert!(full.len() > tight.len());
    assert!(tight.contains("91%"), "got: {tight}");
    assert!(!tight.contains("tok"), "got: {tight}");
    assert!(tight.chars().count() <= 20, "got: {tight}");
}
