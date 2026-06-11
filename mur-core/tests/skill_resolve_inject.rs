//! End-to-end inject tests: load skills with intent/tool_hint, build an
//! inventory, and assert the rendered layer3 body contains resolved tool names.

use mur_common::skill::manifest::{ProcedureStep, SkillManifest};
use mur_common::skill::mcp::{McpRequirement, SkillCapability};
use mur_common::skill::{McpInventory, Resolution, parse_canonical, resolve_step};

fn step(tool: Option<&str>, intent: Option<&str>, hint: Option<&str>) -> ProcedureStep {
    ProcedureStep {
        description: "test step".into(),
        tool: tool.map(String::from),
        intent: intent.map(String::from),
        tool_hint: hint.map(String::from),
        ..Default::default()
    }
}

// ── Resolver-level end-to-end ──

#[test]
fn literal_step_passes_through_with_inventory() {
    let inv = McpInventory::from_tool_names(vec!["browser.navigate".into()]);
    let s = step(Some("browser.navigate"), None, None);
    let r = resolve_step(&s, &[], &inv);
    assert_eq!(
        r,
        Resolution::Literal {
            tool: "browser.navigate".into()
        }
    );
}

#[test]
fn intent_step_resolves_with_matching_inventory() {
    let inv = McpInventory::from_tool_names(vec!["browser.navigate".into()]);
    let reqs = vec![McpRequirement {
        tool_pattern: "browser.*".into(),
        capability: SkillCapability::NetworkHttp,
        fallback: String::new(),
    }];
    let s = step(None, Some("web_navigate"), None);
    let r = resolve_step(&s, &reqs, &inv);
    assert_eq!(
        r,
        Resolution::IntentMatch {
            tool: "browser.navigate".into(),
            capability: SkillCapability::NetworkHttp,
        }
    );
}

// ── Rendered output integration ──

#[test]
fn render_step_shows_tool_name() {
    // Simulates what layer3_body does.
    let s = step(Some("browser.navigate"), None, None);
    let r = resolve_step(&s, &[], &McpInventory::default());
    let rendered = format!("1. {} — tool: {}", s.description, r.picked_tool().unwrap());
    assert_eq!(rendered, "1. test step — tool: browser.navigate");
}

#[test]
fn render_step_shows_no_tool_for_unresolved() {
    let s = step(None, Some("web_navigate"), None);
    let r = resolve_step(&s, &[], &McpInventory::default());
    assert!(r.picked_tool().is_none());
    let rendered = format!(
        "1. {} — (no tool available: {})",
        s.description,
        match &r {
            Resolution::Unresolved { reason } => reason.as_str(),
            _ => "unknown",
        }
    );
    assert!(rendered.contains("no tool available"));
    assert!(rendered.contains("has no matching tool"));
}

// ── v2.2 manifest integration ──

#[test]
fn v22_skill_loads_and_resolves() {
    let yaml = r#"
name: intent-skill
version: 1.0.0
publisher: human:test
description: Skill with intent-based steps
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate to search
        intent: web_navigate
        tool_hint: browser.navigate
      - description: Type query
        tool: legacy-search
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
    let m: SkillManifest = parse_canonical(yaml).unwrap();
    let inv = McpInventory::from_tool_names(vec!["browser.navigate".into()]);
    let proc = m.content.procedure.as_ref().unwrap();

    // Step 0: intent-based, hint matches → Hint resolution
    let r0 = resolve_step(&proc.steps[0], &m.mcp_requirements, &inv);
    assert_eq!(
        r0,
        Resolution::Hint {
            tool: "browser.navigate".into()
        }
    );

    // Step 1: literal tool, no intent → Literal resolution
    let r1 = resolve_step(&proc.steps[1], &m.mcp_requirements, &inv);
    assert_eq!(
        r1,
        Resolution::Literal {
            tool: "legacy-search".into()
        }
    );
}
