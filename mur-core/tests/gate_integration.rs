//! Integration: ack-style queries must skip injection entirely.

use mur_core::retrieve::gate::{GateInputs, Tier, evaluate_query_v2};

#[test]
fn ack_short_words_skip_in_default_inputs() {
    let inputs = GateInputs::default();
    for q in &["ok", "好", "thanks", "符合", "OK!", "嗯", "對"] {
        let o = evaluate_query_v2(q, &inputs);
        assert_eq!(o.tier, Tier::Skip, "query {:?} should skip, got {:?}", q, o);
    }
}

#[test]
fn meta_commands_skip() {
    let inputs = GateInputs::default();
    for q in &["/help", "/status", "/model gpt-4", "/clear"] {
        let o = evaluate_query_v2(q, &inputs);
        assert_eq!(o.tier, Tier::Skip, "query {:?} should skip", q);
    }
}
