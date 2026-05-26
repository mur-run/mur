//! Read-side: load canonical YAML then resolve aliases through IntentLookup.
use std::fs;

use mur_core::cross_agent::intent::canonical::{
    CanonicalEntry, IntentCanonical, write_canonical_yaml,
};
use mur_core::cross_agent::intent::inject_lookup::IntentLookup;
use tempfile::tempdir;

#[test]
fn resolves_aliases_from_yaml_file() {
    let d = tempdir().unwrap();
    let home = d.path();

    let ic = IntentCanonical {
        version: 1,
        generated_at: chrono::Utc::now(),
        generated_by: "test".into(),
        canonical: vec![
            CanonicalEntry {
                canonical: "web_search".into(),
                aliases: vec![
                    "web_search".into(),
                    "search_web".into(),
                    "Web Search".into(),
                ],
                count: 3,
            },
            CanonicalEntry {
                canonical: "run_tests".into(),
                aliases: vec!["run_tests".into(), "Run Tests".into()],
                count: 2,
            },
        ],
    };
    write_canonical_yaml(home, &ic).unwrap();

    // Verify the file was written.
    assert!(home.join("intent_canonical.yaml").exists());

    let lookup = IntentLookup::load(home);

    assert_eq!(lookup.resolve_intent("search_web"), "web_search");
    assert_eq!(lookup.resolve_intent("Web Search"), "web_search");
    assert_eq!(lookup.resolve_intent("Run Tests"), "run_tests");
    assert_eq!(lookup.resolve_intent("run_tests"), "run_tests");
    // Unknown intent passes through.
    assert_eq!(lookup.resolve_intent("novel_intent"), "novel_intent");
}

#[test]
fn missing_yaml_yields_empty_lookup() {
    let d = tempdir().unwrap();
    let home = d.path();

    // No file at all.
    let lookup = IntentLookup::load(home);
    assert_eq!(lookup.resolve_intent("anything"), "anything");
}

#[test]
fn corrupt_yaml_treated_as_missing() {
    let d = tempdir().unwrap();
    let home = d.path();

    fs::write(home.join("intent_canonical.yaml"), "not: valid: yaml: [").unwrap();

    let lookup = IntentLookup::load(home);
    // Should not panic — treats corrupt file as empty.
    assert_eq!(lookup.resolve_intent("anything"), "anything");
}
