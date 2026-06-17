//! Unit-style integration tests for `mur model` CLI helpers.
//!
//! Tests call `mur_core::cmd::model::build_entry_costs` directly so they run
//! without spawning the `mur` binary.

use mur_common::model::ModelEntry;
use mur_core::cmd::model::build_entry_costs;

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
