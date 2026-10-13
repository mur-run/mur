//! Asserts every future profile change still loads v1_frozen.yaml.
//! See R12 in docs/superpowers/specs/2026-04-29-mur-companion-phase-1-1-design.md.

use mur_common::agent::AgentProfile;

#[test]
fn v1_frozen_profile_still_loads() {
    let yaml = std::fs::read_to_string("../tests/fixtures/profile/v1_frozen.yaml")
        .expect("v1_frozen.yaml must exist — DO NOT DELETE OR EDIT");
    let _p: AgentProfile = serde_yaml_ng::from_str(&yaml)
        .expect("v1_frozen profile must remain readable across all future schema changes");
}

/// #1713: an older binary that rewrites a profile must not drop keys it does
/// not know. `kind: serena` tightens an MCP server's gates, so losing it makes
/// the entry *less* constrained — silent loss is the wrong failure direction.
#[test]
fn unknown_profile_keys_survive_a_round_trip() {
    let base = std::fs::read_to_string("../tests/fixtures/profile/v1_frozen.yaml")
        .expect("v1_frozen.yaml must exist — DO NOT DELETE OR EDIT");
    let yaml = base.replace(
        "mcp_servers: []",
        "future_top_level: { nested: [1, 2, 3] }\nmcp_servers:\n  - name: serena\n    command: serena\n    future_kind: strict\n    future_map: { a: b }",
    );
    assert!(
        yaml.contains("future_kind"),
        "fixture no longer has mcp_servers: []"
    );
    let p: AgentProfile = serde_yaml_ng::from_str(&yaml).expect("parses");
    let out = serde_yaml_ng::to_string(&p).expect("serialises");
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();

    assert_eq!(
        v["future_top_level"]["nested"][2],
        serde_yaml_ng::Value::from(3),
        "unknown top-level key lost:\n{out}"
    );
    let entry = v["mcp_servers"]
        .as_sequence()
        .and_then(|s| s.iter().find(|e| e["name"] == "serena"))
        .unwrap_or_else(|| panic!("serena entry missing:\n{out}"));
    assert_eq!(
        entry["future_kind"], "strict",
        "unknown MCP key lost:\n{out}"
    );
    assert_eq!(entry["future_map"]["a"], "b");
}
