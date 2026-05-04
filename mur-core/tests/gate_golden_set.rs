//! Golden set accuracy test: ≥ 85% must hit the expected tier.

use mur_core::retrieve::gate::{evaluate_query_v2, GateInputs, Tier};
use serde::Deserialize;

#[derive(Deserialize)]
struct Row {
    query: String,
    expected_tier: String,
}

fn parse_tier(s: &str) -> Tier {
    match s {
        "Skip" => Tier::Skip,
        "L0" => Tier::L0,
        "L1" => Tier::L1,
        "L2" => Tier::L2,
        other => panic!("unknown tier in golden set: {other}"),
    }
}

#[test]
fn golden_set_accuracy_at_least_85_percent() {
    let raw = std::fs::read_to_string("tests/fixtures/gate_golden_set.jsonl")
        .expect("missing fixture");
    let rows: Vec<Row> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad row {l}: {e}")))
        .collect();

    assert_eq!(rows.len(), 100, "fixture must contain 100 rows");

    let inputs = GateInputs::default();
    let mut hits = 0;
    let mut misses: Vec<(String, Tier, Tier)> = Vec::new();

    for row in &rows {
        let expected = parse_tier(&row.expected_tier);
        let actual = evaluate_query_v2(&row.query, &inputs).tier;
        if actual == expected {
            hits += 1;
        } else {
            misses.push((row.query.clone(), expected, actual));
        }
    }

    let accuracy = hits as f32 / rows.len() as f32;
    if accuracy < 0.85 {
        for (q, want, got) in &misses {
            eprintln!("MISS: {q:?} want={want:?} got={got:?}");
        }
        panic!("golden set accuracy {:.2} < 0.85 ({} hits / {} total)", accuracy, hits, rows.len());
    }
}

#[test]
fn golden_set_skip_recall_perfect() {
    let raw = std::fs::read_to_string("tests/fixtures/gate_golden_set.jsonl").unwrap();
    let rows: Vec<Row> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    let inputs = GateInputs::default();
    for row in &rows {
        if row.expected_tier == "Skip" {
            let actual = evaluate_query_v2(&row.query, &inputs).tier;
            assert_eq!(actual, Tier::Skip, "row {:?} must skip but got {:?}", row.query, actual);
        }
    }
}
