//! `session_cost_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;

#[test]
fn session_cost_uses_pricing_over_session_tokens_or_none() {
    let mut a = App::test_fixture();
    a.pricing = super::super::super::footer::Pricing {
        in_per_1k: Some(3.0),
        out_per_1k: Some(15.0),
        window: None,
    };
    a.session_in = 1000;
    a.session_out = 1000;
    // (1000/1000*3) + (1000/1000*15) = 18.0
    assert_eq!(a.session_cost(), Some(18.0));
    // unpriced model → None (fail-open: unknown cost never blocks)
    a.pricing = super::super::super::footer::Pricing::default();
    assert_eq!(a.session_cost(), None);
}
