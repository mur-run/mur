//! Unit-style integration tests for `mur model` CLI helpers.
//!
//! Tests call `mur_core::cmd::model::build_entry_costs` directly so they run
//! without spawning the `mur` binary.

use mur_common::model::ModelEntry;
use mur_core::cmd::model::{apply_fetched_prices, build_entry_costs};
use mur_core::model_prices::PriceInfo;

#[test]
fn cost_flags_map_to_fields() {
    // --input-cost and --output-cost both provided: each goes to its own field.
    let e = build_entry_costs(ModelEntry::default(), Some(0.003), Some(0.015), None);
    assert_eq!(e.input_cost_per_1k, Some(0.003));
    assert_eq!(e.output_cost_per_1k, Some(0.015));
    assert_eq!(e.cost_per_1k_tokens, None); // deprecated field untouched when --output-cost used

    // --cost-per-1k only: maps to output slot + deprecated field (back-compat).
    let e2 = build_entry_costs(ModelEntry::default(), None, None, Some(0.012));
    assert_eq!(e2.input_cost_per_1k, None);
    assert_eq!(e2.output_cost_per_1k, Some(0.012));
    assert_eq!(e2.cost_per_1k_tokens, Some(0.012));

    // --output-cost wins over --cost-per-1k when both supplied.
    let e3 = build_entry_costs(ModelEntry::default(), None, Some(0.020), Some(0.012));
    assert_eq!(e3.output_cost_per_1k, Some(0.020));
    // deprecated field is not set when --output-cost is present
    assert_eq!(e3.cost_per_1k_tokens, None);

    // No cost flags: all None.
    let e4 = build_entry_costs(ModelEntry::default(), None, None, None);
    assert_eq!(e4.input_cost_per_1k, None);
    assert_eq!(e4.output_cost_per_1k, None);
    assert_eq!(e4.cost_per_1k_tokens, None);
}

#[test]
fn fetched_prices_fill_only_empty_fields() {
    // entry has output_cost set explicitly; input_cost and context_window are None.
    let e = build_entry_costs(ModelEntry::default(), None, Some(0.99), None);
    let filled = apply_fetched_prices(
        e,
        Some(PriceInfo {
            input_per_1k: 0.005,
            output_per_1k: 0.025,
            context_window: Some(200_000),
        }),
    );
    // Explicit output_cost must not be overwritten.
    assert_eq!(filled.output_cost_per_1k, Some(0.99));
    // input_cost was None — should be filled from fetched.
    assert_eq!(filled.input_cost_per_1k, Some(0.005));
    // context_window was None — should be filled from fetched.
    assert_eq!(filled.context_window, Some(200_000));
}

#[test]
fn fetched_prices_none_is_noop() {
    let e = build_entry_costs(ModelEntry::default(), Some(0.003), Some(0.015), None);
    let unchanged = apply_fetched_prices(e.clone(), None);
    assert_eq!(unchanged.input_cost_per_1k, e.input_cost_per_1k);
    assert_eq!(unchanged.output_cost_per_1k, e.output_cost_per_1k);
    assert_eq!(unchanged.context_window, e.context_window);
}
