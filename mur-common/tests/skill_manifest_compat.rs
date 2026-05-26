//! Backward-compat tests for schema evolution (v2.0 → v2.1 → v2.2).
//!
//! Verifies that older manifests load correctly with new optional fields
//! defaulting to empty, and that serialization is byte-identical so
//! publisher signatures remain valid.

use mur_common::skill::manifest::{Skill, SkillManifest};
use mur_common::skill::validate;
use mur_common::skill::parse_canonical;

#[test]
fn v20_manifest_loads_with_default_empty_requirements() {
    let yaml = r#"
name: legacy
version: 1.0.0
publisher: human:test
description: A v2.0 manifest without mcp_requirements
category: context
content:
  abstract: test
  context: test context
"#;
    let skill: Skill = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(skill.manifest.mcp_requirements.len(), 0);

    // Round-trip: the deserialized skill should still have empty requirements.
    let out = serde_yaml_ng::to_string(&skill).unwrap();
    let skill2: Skill = serde_yaml_ng::from_str(&out).unwrap();
    assert_eq!(skill2.manifest.mcp_requirements.len(), 0);
}

#[test]
fn v20_manifest_byte_identical_after_round_trip() {
    let yaml = r#"
name: legacy
version: 1.0.0
publisher: human:test
description: A v2.0 manifest without mcp_requirements
category: context
content:
  abstract: test
  context: test context
"#;
    let skill: Skill = serde_yaml_ng::from_str(yaml).unwrap();
    let out = serde_yaml_ng::to_string(&skill).unwrap();

    // Re-parse and re-serialize — the second serialization should be
    // identical to the first (no new fields leaked).
    let skill2: Skill = serde_yaml_ng::from_str(&out).unwrap();
    let out2 = serde_yaml_ng::to_string(&skill2).unwrap();
    assert_eq!(out, out2, "v2.0 manifest round-trip must be byte-identical");
}

#[test]
fn v21_manifest_with_requirements_round_trips() {
    let yaml = r#"
name: mcp-skill
version: 1.0.0
publisher: human:test
description: Uses MCP tools
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: search the web
        tool: browser.navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
  - tool_pattern: "filesystem.write.*"
    capability: write_file
    fallback: builtin-touch
"#;
    let skill: Skill = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(skill.manifest.mcp_requirements.len(), 2);
    assert_eq!(skill.manifest.mcp_requirements[0].tool_pattern, "browser.*");
    assert_eq!(
        skill.manifest.mcp_requirements[0].capability.to_string(),
        "network_http"
    );
    assert_eq!(skill.manifest.mcp_requirements[1].fallback, "builtin-touch");

    // Round-trip
    let out = serde_yaml_ng::to_string(&skill).unwrap();
    let skill2: Skill = serde_yaml_ng::from_str(&out).unwrap();
    assert_eq!(skill2.manifest.mcp_requirements.len(), 2);
    assert_eq!(
        skill2.manifest.mcp_requirements[0].tool_pattern,
        "browser.*"
    );
    assert_eq!(
        skill2.manifest.mcp_requirements[1].fallback,
        "builtin-touch"
    );
}

#[test]
fn unknown_capability_string_rejected() {
    let yaml = r#"
name: bad
version: 1.0.0
publisher: human:test
description: bad capability
category: context
content:
  abstract: test
  context: test context
mcp_requirements:
  - tool_pattern: "x.*"
    capability: telepathy
"#;
    let err = serde_yaml_ng::from_str::<Skill>(yaml).unwrap_err();
    assert!(
        err.to_string().contains("unknown MCP capability"),
        "expected 'unknown MCP capability' error, got: {err}"
    );
    assert!(
        err.to_string().contains("telepathy"),
        "error should name the bad capability, got: {err}"
    );
}

#[test]
fn empty_mcp_requirements_serializes_without_field() {
    let yaml = r#"
name: no-mcp
version: 1.0.0
publisher: human:test
description: Explicitly empty requirements
category: context
content:
  abstract: test
  context: test context
mcp_requirements: []
"#;
    let skill: Skill = serde_yaml_ng::from_str(yaml).unwrap();
    assert!(skill.manifest.mcp_requirements.is_empty());

    // Explicitly empty list should be serialized out (skip_serializing_if).
    let out = serde_yaml_ng::to_string(&skill).unwrap();
    assert!(
        !out.contains("mcp_requirements"),
        "empty mcp_requirements should be skipped on serialization"
    );
}

// ── v2.2: intent + tool_hint ──

#[test]
fn v22_manifest_with_intent_and_hint_loads() {
    let yaml = r#"
name: intent-skill
version: 1.0.0
publisher: human:test
description: Uses intent-based tool resolution
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate to search page
        intent: web_navigate
        tool_hint: browser.navigate
      - description: Click first result
        intent: web_click
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
    let skill: Skill = serde_yaml_ng::from_str(yaml).unwrap();
    let proc = skill.manifest.content.procedure.as_ref().unwrap();
    assert_eq!(proc.steps.len(), 2);
    assert_eq!(proc.steps[0].intent.as_deref(), Some("web_navigate"));
    assert_eq!(proc.steps[0].tool_hint.as_deref(), Some("browser.navigate"));
    assert_eq!(proc.steps[1].intent.as_deref(), Some("web_click"));
    assert_eq!(proc.steps[1].tool_hint.as_deref(), None);

    // Validate passes.
    validate(&skill.manifest).unwrap();
}

#[test]
fn v22_manifest_round_trips() {
    let yaml = r#"
name: intent-skill
version: 1.0.0
publisher: human:test
description: Round-trip test
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        intent: web_navigate
        tool_hint: browser.navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
    let skill: Skill = serde_yaml_ng::from_str(yaml).unwrap();
    let out = serde_yaml_ng::to_string(&skill).unwrap();
    let skill2: Skill = serde_yaml_ng::from_str(&out).unwrap();
    let proc = skill2.manifest.content.procedure.as_ref().unwrap();
    assert_eq!(proc.steps[0].intent.as_deref(), Some("web_navigate"));
    assert_eq!(proc.steps[0].tool_hint.as_deref(), Some("browser.navigate"));
}

#[test]
fn v22_manifest_without_intent_serializes_cleanly() {
    // Steps with only description + tool (no intent) should not emit
    // intent or tool_hint in the serialized output.
    let yaml = r#"
name: plain-step
version: 1.0.0
publisher: human:test
description: Classic tool-only step
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        tool: browser.navigate
"#;
    let skill: Skill = serde_yaml_ng::from_str(yaml).unwrap();
    let out = serde_yaml_ng::to_string(&skill).unwrap();
    assert!(!out.contains("intent"), "unexpected 'intent' in output: {out}");
    assert!(!out.contains("tool_hint"), "unexpected 'tool_hint' in output: {out}");
}

#[test]
fn v22_rejects_empty_intent() {
    let yaml = r#"
name: bad-intent
version: 1.0.0
publisher: human:test
description: Empty intent
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
        intent: ""
"#;
    let m = parse_canonical(yaml).unwrap();
    let err = validate(&m).unwrap_err();
    assert!(
        err.to_string().contains("intent must not be empty"),
        "expected 'intent must not be empty', got: {err}"
    );
}

#[test]
fn v22_rejects_empty_tool_hint() {
    let yaml = r#"
name: bad-hint
version: 1.0.0
publisher: human:test
description: Empty tool_hint
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
        tool_hint: ""
"#;
    let m = parse_canonical(yaml).unwrap();
    let err = validate(&m).unwrap_err();
    assert!(
        err.to_string().contains("tool_hint must not be empty"),
        "expected 'tool_hint must not be empty', got: {err}"
    );
}

#[test]
fn v22_step_with_only_tool_still_valid() {
    // Pre-M6b steps (tool only, no intent) must still pass validation.
    let yaml = r#"
name: classic
version: 1.0.0
publisher: human:test
description: Classic tool-only step
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        tool: browser.navigate
"#;
    let m = parse_canonical(yaml).unwrap();
    validate(&m).unwrap(); // must not panic
}

#[test]
fn v22_v20_byte_identical_round_trip_preserved() {
    // v2.0 manifests must still round-trip byte-identically even after
    // the v2.2 schema changes (no leaked fields).
    let yaml = r#"
name: legacy
version: 1.0.0
publisher: human:test
description: A v2.0 manifest without mcp_requirements
category: context
content:
  abstract: test
  context: test context
"#;
    let skill: Skill = serde_yaml_ng::from_str(yaml).unwrap();
    let out = serde_yaml_ng::to_string(&skill).unwrap();
    let skill2: Skill = serde_yaml_ng::from_str(&out).unwrap();
    let out2 = serde_yaml_ng::to_string(&skill2).unwrap();
    assert_eq!(out, out2, "v2.0 byte-identical round-trip must hold across v2.2");
}

#[test]
fn manifest_deserializes_skillcapability_from_yaml() {
    let yaml = r#"
name: direct-cap
version: 1.0.0
publisher: human:test
description: Direct SkillCapability deserialization
category: context
content:
  abstract: test
  context: test context
mcp_requirements:
  - tool_pattern: "*.search"
    capability: search
"#;
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(m.mcp_requirements.len(), 1);
    assert_eq!(m.mcp_requirements[0].capability.to_string(), "search");
}
