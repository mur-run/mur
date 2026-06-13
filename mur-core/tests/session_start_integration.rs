//! Verifies that inject::index correctly builds and formats an L0 index.

use mur_core::inject::index::{CapabilityEntry, CapabilityIndex, format_l0};

#[test]
fn l0_output_fits_within_600_token_budget() {
    let entries: Vec<_> = (0..30)
        .map(|i| CapabilityEntry {
            name: format!("pattern-{i}"),
            description: format!(
                "A test description for pattern number {i} that is moderately long"
            ),
        })
        .collect();
    let idx = CapabilityIndex {
        entries,
        project: Some("myproject".into()),
    };
    let out = format_l0(&idx, 2400);
    assert!(out.len() <= 2600, "L0 output too long: {} chars", out.len());
    assert!(out.contains("## mur learning index"));
    assert!(out.contains("project: myproject"));
    assert!(out.contains("mur skill show"));
}

#[test]
fn l0_output_has_correct_format_per_entry() {
    let idx = CapabilityIndex {
        entries: vec![CapabilityEntry {
            name: "tokio-async-runtime".into(),
            description: "Tokio: spawn / select! / time::sleep".into(),
        }],
        project: None,
    };
    let out = format_l0(&idx, 4000);
    assert!(
        out.contains("- `tokio-async-runtime` — Tokio: spawn / select! / time::sleep"),
        "entry line format must be: - `name` — description\ngot:\n{out}"
    );
}

#[test]
fn empty_index_produces_no_output() {
    let idx = CapabilityIndex {
        entries: vec![],
        project: None,
    };
    assert_eq!(format_l0(&idx, 4000), "");
}
