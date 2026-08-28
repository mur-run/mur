//! End-to-end: install skills → boot-inject → trigger match → Layer-3 swap.
//! Tests up to the injection assembly boundary.

use mur_agent_runtime::skills::RuntimeSkills;
use mur_agent_runtime::skills::injector::inject_layer2;
use mur_agent_runtime::skills::trigger_matcher::{format_layer3, layer3_body, match_prompt};
use mur_common::config::SkillsConfig;
use mur_common::skill::loader::load_all;
use mur_common::skill::{McpInventory, parse_canonical, write_to_dir};
use std::collections::HashSet;
use tempfile::tempdir;

#[test]
fn boot_inject_trigger_swap_e2e() {
    let dir = tempdir().unwrap();
    let mur_home = dir.path();

    // Skill A: session_start only → always injected as Layer 2
    let a = parse_canonical(
        r#"
name: house-rules
version: 1.0.0
publisher: human:t
description: always-on rules
category: context
content:
  abstract: Always reply concisely.
  context: "Long body for house-rules."
triggers:
  - type: session_start
"#,
    )
    .unwrap();

    // Skill B: command → Layer 3 swap path
    let b = parse_canonical(
        r#"
name: find-price
version: 1.0.0
publisher: human:t
description: find prices
category: context
content:
  abstract: Searches product prices.
  context: "Full procedure: navigate, search, extract."
triggers:
  - type: command
    pattern: /find-price
"#,
    )
    .unwrap();

    write_to_dir(&mur_home.join("skills").join("house-rules"), &a).unwrap();
    write_to_dir(&mur_home.join("skills").join("find-price"), &b).unwrap();

    let loaded = load_all(mur_home, "alice");
    assert_eq!(loaded.len(), 2);

    let runtime = RuntimeSkills::build(loaded);
    let runtime = runtime.snapshot();

    // (1) Layer 2 contains house-rules, not find-price (no session_start).
    let inj = inject_layer2(
        &runtime.loaded,
        &SkillsConfig::default(),
        &mur_common::config::MemoryConfig::default(),
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );
    assert!(inj.system_addendum.contains("house-rules"));
    assert!(!inj.system_addendum.contains("find-price"));

    // (2) Trigger matcher finds find-price for "/find-price airpods".
    let matched = match_prompt(&runtime.triggers, "/find-price airpods");
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].skill_name, "find-price");

    // (3) Layer 3 body + formatting end-to-end.
    let body = layer3_body(
        &runtime
            .loaded
            .iter()
            .find(|s| s.name == "find-price")
            .unwrap()
            .manifest,
        &McpInventory::default(),
    )
    .unwrap();
    let wrapped = format_layer3("find-price", matched[0].trust, &body);
    assert!(wrapped.starts_with("<skill-instruction source=\"find-price\""));
    assert!(wrapped.contains("Full procedure"));
}
