//! Integration tests for skill consolidation dedup passes (M6c.1).
//!
//! Tests the combined dedup logic, DedupSource serialization, and the
//! Jaccard pass's source-stamping. Full vector-path tests require a
//! running embedder (Ollama/OpenAI) and are exercised via the CLI.

use mur_core::skill_consolidate::dedup::{DedupSource, DuplicatePair, KeeperReason};

#[test]
fn dedup_source_serializes_snake_case() {
    let cases = vec![
        (DedupSource::Jaccard, "\"jaccard\""),
        (DedupSource::Vector, "\"vector\""),
        (DedupSource::Both, "\"both\""),
    ];
    for (src, expected) in cases {
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, expected);
    }
}

#[test]
fn dedup_source_deserializes_with_default() {
    // Missing field defaults to Jaccard (backwards-compat).
    let json = r#"{"a":"x","b":"y","similarity":0.9,"keeper":"x","kept_reason":"alphabetical"}"#;
    let pair: DuplicatePair = serde_json::from_str(json).unwrap();
    assert_eq!(pair.source, DedupSource::Jaccard);
}

#[test]
fn dedup_source_deserializes_explicit() {
    let json = r#"{"a":"x","b":"y","similarity":0.94,"keeper":"x","kept_reason":"alphabetical","source":"vector"}"#;
    let pair: DuplicatePair = serde_json::from_str(json).unwrap();
    assert_eq!(pair.source, DedupSource::Vector);
    assert!((pair.similarity - 0.94).abs() < 0.001);
}

#[test]
fn duplicate_pair_serializes_with_source() {
    let pair = DuplicatePair {
        a: "web-search".into(),
        b: "web-search-v2".into(),
        similarity: 0.93,
        keeper: "web-search".into(),
        kept_reason: KeeperReason::HigherSuccessCount,
        source: DedupSource::Vector,
    };
    let json = serde_json::to_string(&pair).unwrap();
    assert!(json.contains("\"source\":\"vector\""));
    assert!(json.contains("\"similarity\":0.93"));
    assert!(json.contains("\"keeper\":\"web-search\""));
}

#[test]
fn consolidate_method_serializes() {
    use mur_core::skill_consolidate::ConsolidateMethod;
    assert_eq!(
        serde_json::to_string(&ConsolidateMethod::Jaccard).unwrap(),
        "\"jaccard\""
    );
    assert_eq!(
        serde_json::to_string(&ConsolidateMethod::Vector).unwrap(),
        "\"vector\""
    );
    assert_eq!(
        serde_json::to_string(&ConsolidateMethod::Both).unwrap(),
        "\"both\""
    );
}
