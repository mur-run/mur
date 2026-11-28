//! `session_cost_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;

#[test]
fn session_cost_accrues_each_turn_at_its_pricing_or_none() {
    let mut a = App::test_fixture();
    // Nothing priced yet → None (fail-open: unknown cost never blocks).
    a.apply_usage(&serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000 }));
    assert_eq!(a.session_cost(), None);
    a.pricing = super::super::super::footer::Pricing {
        in_per_1k: Some(3.0),
        out_per_1k: Some(15.0),
        window: None,
        billing: None,
    };
    // (1000/1000*3) + (1000/1000*15) = 18.0; the earlier unpriced turn adds 0.
    a.apply_usage(&serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000 }));
    assert_eq!(a.session_cost(), Some(18.0));
    a.apply_usage(&serde_json::json!({ "input_tokens": 1000, "output_tokens": 0 }));
    assert_eq!(a.session_cost(), Some(21.0));
}
