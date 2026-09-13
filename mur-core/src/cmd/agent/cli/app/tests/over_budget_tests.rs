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
    };
    a.session_in = 1000;
    a.session_out = 1000; // $18.00 spent
    a.budget_usd = None;
    assert!(!a.over_budget()); // no cap
    a.budget_usd = Some(20.0);
    assert!(!a.over_budget()); // under
    a.budget_usd = Some(18.0);
    assert!(a.over_budget()); // at cap
    a.budget_usd = Some(5.0);
    assert!(a.over_budget()); // over
    a.pricing = super::super::super::footer::Pricing::default(); // unpriced → fail OPEN
    a.budget_usd = Some(0.01);
    assert!(!a.over_budget());
}
