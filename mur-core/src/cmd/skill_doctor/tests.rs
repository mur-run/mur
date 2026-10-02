use super::*;
use std::fs;
use tempfile::TempDir;

fn write_skill(dir: &TempDir, name: &str, yaml: &str) {
    let skill_dir = dir.path().join("skills").join(name);
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("skill.yaml"), yaml).unwrap();
}

fn doctor_ctx(dir: &TempDir) -> DoctorCtx {
    DoctorCtx {
        home: dir.path().to_path_buf(),
        now: chrono::Utc::now(),
        installed_skills: std::collections::HashSet::new(),
        mcp_tools: None,
        llm_enabled: false,
        llm_ctx: None,
    }
}

fn doctor_ctx_with_tools(dir: &TempDir, tools: Vec<String>) -> DoctorCtx {
    DoctorCtx {
        home: dir.path().to_path_buf(),
        now: chrono::Utc::now(),
        installed_skills: std::collections::HashSet::new(),
        mcp_tools: Some(tools),
        llm_enabled: false,
        llm_ctx: None,
    }
}

fn write_shadow_skill(dir: &std::path::Path, name: &str, abstract_text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let yaml = format!(
        "name: {name}\nversion: 1.0.0\npublisher: human:test\ndescription: 'test skill {name}'\ncategory: context\ncontent:\n  abstract: '{abstract_text}'\n  context: 'body'\n"
    );
    std::fs::write(dir.join("skill.yaml"), yaml).unwrap();
}

#[test]
fn shadow_drift_flags_diverged_agent_copy_as_warn() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    write_shadow_skill(&home.join("skills/foo"), "foo", "global version");
    write_shadow_skill(
        &home.join("agents/a1/skills/foo"),
        "foo",
        "agent-diverged version",
    );

    let mut ctx = doctor_ctx(&tmp);
    ctx.installed_skills.insert("foo".to_string());

    let findings = run_shadow_drift(&ctx);
    let f = findings
        .iter()
        .find(|f| f.skill_name == "foo")
        .expect("expected a shadow finding for foo");
    assert_eq!(f.check_id, "shadow-drift");
    assert_eq!(f.severity, Severity::Warn);
    assert_eq!(
        f.remediation.as_deref().unwrap(),
        "mur agent skill remove a1 foo",
        "remediation must be the basename form"
    );
    assert!(!f.fixable, "diverged shadow must NOT be auto-fixable");
}

#[test]
fn shadow_drift_flags_identical_agent_copy_as_ok() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    write_shadow_skill(&home.join("skills/foo"), "foo", "same version");
    write_shadow_skill(&home.join("agents/a1/skills/foo"), "foo", "same version");

    let mut ctx = doctor_ctx(&tmp);
    ctx.installed_skills.insert("foo".to_string());

    let findings = run_shadow_drift(&ctx);
    let f = findings
        .iter()
        .find(|f| f.skill_name == "foo")
        .expect("expected a shadow finding for foo");
    assert_eq!(f.severity, Severity::Ok);
    assert!(f.fixable, "identical shadow must be auto-fixable");
    assert_eq!(
        f.remediation.as_deref().unwrap(),
        "mur agent skill remove a1 foo",
        "remediation must be the basename form depin_skill resolves"
    );
}

#[test]
fn shadow_drift_ignores_agent_only_skill() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    // agent-local skill with NO global twin — legitimate, must not be flagged
    write_shadow_skill(
        &home.join("agents/a1/skills/private"),
        "private",
        "agent only",
    );

    let ctx = doctor_ctx(&tmp); // installed_skills empty
    let findings = run_shadow_drift(&ctx);
    assert!(
        findings.is_empty(),
        "agent-only skills must not be flagged as shadows"
    );
}

#[test]
fn coverage_workflow_with_dotted_tools_no_requirements() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: browser-skill
version: 1.0.0
publisher: human:test
description: Skill with tool refs but no mcp_requirements
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: navigate
        tool: browser.navigate
      - description: search
        tool: browser.search
"#;
    write_skill(&dir, "browser-skill", yaml);
    let ctx = doctor_ctx(&dir);
    let findings = run_mcp_requirements_coverage(&ctx, "browser-skill");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].check_id, "mcp-requirements-coverage");
    assert_eq!(findings[0].severity, Severity::Warn);
    assert!(findings[0].message.contains("browser.navigate"));
}

#[test]
fn coverage_workflow_with_requirements_no_finding() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: covered-skill
version: 1.0.0
publisher: human:test
description: Skill with tool refs and mcp_requirements
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: navigate
        tool: browser.navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
    write_skill(&dir, "covered-skill", yaml);
    let ctx = doctor_ctx(&dir);
    let findings = run_mcp_requirements_coverage(&ctx, "covered-skill");
    assert!(
        findings.is_empty(),
        "expected no findings, got {findings:?}"
    );
}

#[test]
fn coverage_context_mode_skipped() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: context-skill
version: 1.0.0
publisher: human:test
description: Context skill with no procedure
category: context
content:
  abstract: test
  context: some context
"#;
    write_skill(&dir, "context-skill", yaml);
    let ctx = doctor_ctx(&dir);
    let findings = run_mcp_requirements_coverage(&ctx, "context-skill");
    assert!(findings.is_empty());
}

#[test]
fn coverage_no_dotted_tools_no_finding() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: no-tools
version: 1.0.0
publisher: human:test
description: Workflow with no dotted tool refs
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: do something
"#;
    write_skill(&dir, "no-tools", yaml);
    let ctx = doctor_ctx(&dir);
    let findings = run_mcp_requirements_coverage(&ctx, "no-tools");
    assert!(findings.is_empty());
}

// ── mcp-capability-available ──

#[test]
fn capability_check_unknown_without_agent_context() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: mcp-skill
version: 1.0.0
publisher: human:test
description: Skill with MCP requirements
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
    write_skill(&dir, "mcp-skill", yaml);
    let ctx = doctor_ctx(&dir); // mcp_tools = None
    let findings = run_mcp_capability_available(&ctx, "mcp-skill");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].check_id, "mcp-capability-available");
    assert_eq!(findings[0].severity, Severity::Unknown);
}

#[test]
fn capability_check_warns_when_no_match() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: browser-skill
version: 1.0.0
publisher: human:test
description: Skill needing browser
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
    write_skill(&dir, "browser-skill", yaml);
    let ctx = doctor_ctx_with_tools(&dir, vec!["filesystem.read".into(), "search.google".into()]);
    let findings = run_mcp_capability_available(&ctx, "browser-skill");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Warn);
    assert!(findings[0].message.contains("browser.*"));
}

#[test]
fn capability_ok_when_glob_matches() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: browser-skill
version: 1.0.0
publisher: human:test
description: Skill needing browser
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
    write_skill(&dir, "browser-skill", yaml);
    let ctx = doctor_ctx_with_tools(
        &dir,
        vec!["browser.navigate".into(), "browser.screenshot".into()],
    );
    let findings = run_mcp_capability_available(&ctx, "browser-skill");
    assert!(
        findings.is_empty(),
        "expected no findings, got {findings:?}"
    );
}

#[test]
fn capability_skips_fallback_requirement() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: fallback-skill
version: 1.0.0
publisher: human:test
description: Skill with fallback
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
    fallback: builtin-http
"#;
    write_skill(&dir, "fallback-skill", yaml);
    // No browser tools available — but fallback is set, so skip.
    let ctx = doctor_ctx_with_tools(&dir, vec!["filesystem.read".into()]);
    let findings = run_mcp_capability_available(&ctx, "fallback-skill");
    assert!(
        findings.is_empty(),
        "fallback requirements should be skipped, got {findings:?}"
    );
}

#[test]
fn capability_empty_requirements_no_finding() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: simple-skill
version: 1.0.0
publisher: human:test
description: No MCP requirements
category: context
content:
  abstract: test
  context: body
"#;
    write_skill(&dir, "simple-skill", yaml);
    let ctx = doctor_ctx_with_tools(&dir, vec!["browser.navigate".into()]);
    let findings = run_mcp_capability_available(&ctx, "simple-skill");
    assert!(findings.is_empty());
}

// ── intent-resolvable ──

#[test]
fn intent_resolvable_matched_by_inventory() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: intent-skill
version: 1.0.0
publisher: human:test
description: Intent matched by inventory
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        intent: web_navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
    write_skill(&dir, "intent-skill", yaml);
    let ctx = doctor_ctx_with_tools(
        &dir,
        vec!["browser.navigate".into(), "browser.click".into()],
    );
    let findings = run_intent_resolvable(&ctx, "intent-skill");
    assert!(
        findings.is_empty(),
        "expected no findings when intent matches, got {findings:?}"
    );
}

#[test]
fn intent_resolvable_warns_when_no_match() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: unresolvable-skill
version: 1.0.0
publisher: human:test
description: Intent with no matching inventory
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        intent: web_navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
    write_skill(&dir, "unresolvable-skill", yaml);
    let ctx = doctor_ctx_with_tools(&dir, vec!["filesystem.read".into()]);
    let findings = run_intent_resolvable(&ctx, "unresolvable-skill");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Warn);
    assert!(findings[0].message.contains("web_navigate"));
    assert!(findings[0].message.contains("unresolvable"));
}

#[test]
fn intent_resolvable_fallback_in_inventory_no_warning() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: fallback-skill
version: 1.0.0
publisher: human:test
description: Intent resolved via fallback
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        intent: web_navigate
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
    fallback: builtin-http
"#;
    write_skill(&dir, "fallback-skill", yaml);
    let ctx = doctor_ctx_with_tools(&dir, vec!["builtin-http".into()]);
    let findings = run_intent_resolvable(&ctx, "fallback-skill");
    assert!(
        findings.is_empty(),
        "fallback in inventory should resolve, got {findings:?}"
    );
}

#[test]
fn intent_resolvable_skips_steps_without_intent() {
    let dir = TempDir::new().unwrap();
    let yaml = r#"
name: literal-skill
version: 1.0.0
publisher: human:test
description: Only literal tools, no intents
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate
        tool: browser.navigate
      - description: Search
"#;
    write_skill(&dir, "literal-skill", yaml);
    let ctx = doctor_ctx_with_tools(&dir, vec![]);
    let findings = run_intent_resolvable(&ctx, "literal-skill");
    assert!(
        findings.is_empty(),
        "steps without intent should be skipped, got {findings:?}"
    );
}

fn manifest(desc: &str, abstract_: &str) -> mur_common::skill::manifest::SkillManifest {
    let yaml = format!(
        r#"name: t
version: 0.1.0
publisher: human:t
description: "{desc}"
category: context
content:
  abstract: "{abstract_}"
  context: b
"#
    );
    mur_common::skill::parse_canonical(&yaml).unwrap()
}

#[test]
fn disclosure_flags_fat_description_and_abstract() {
    let fat_desc = "d".repeat(121);
    let fat_abs = vec!["word"; 51].join(" ");
    let f = disclosure_findings(&manifest(&fat_desc, &fat_abs), "t");
    assert_eq!(f.len(), 2);
    assert!(
        f.iter()
            .all(|x| x.check_id == "disclosure" && x.severity == Severity::Warn)
    );

    let ok = disclosure_findings(&manifest("short", "brief abstract"), "t");
    assert!(ok.is_empty());
}
