//! Backward-compat tests for M6a schema evolution (v2.0 → v2.1).
//!
//! Verifies that M3-era v2.0 manifests load correctly with the new
//! `mcp_requirements` field defaulting to empty, and that serialization
//! is byte-identical so publisher signatures remain valid.

use mur_common::skill::manifest::{Skill, SkillManifest};

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
    assert_eq!(
        skill.manifest.mcp_requirements[1].fallback,
        "builtin-touch"
    );

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
    assert_eq!(
        m.mcp_requirements[0].capability.to_string(),
        "search"
    );
}
