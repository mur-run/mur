use super::*;

#[test]
fn visibility_defaults_to_indexed_and_parses_on_demand() {
    let yaml = r#"
name: vis-default
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: test
"#;
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(m.visibility, Visibility::Indexed);

    let yaml2 = format!("{yaml}visibility: on_demand\n");
    let m2: SkillManifest = serde_yaml_ng::from_str(&yaml2).unwrap();
    assert_eq!(m2.visibility, Visibility::OnDemand);

    // Default is omitted on serialize (keeps existing manifests signature-stable).
    let out = serde_yaml_ng::to_string(&m).unwrap();
    assert!(!out.contains("visibility"));
    let out2 = serde_yaml_ng::to_string(&m2).unwrap();
    assert!(out2.contains("visibility: on_demand"));
}

#[test]
fn procedure_step_dag_fields_roundtrip() {
    let yaml = r#"
description: deploy the app
command: "fly deploy --app {{app_name}}"
id: deploy
depends_on: [build, test]
on_failure: retry
retry:
  max_retries: 2
  backoff_secs: 5
timeout_secs: 300
needs_approval: true
"#;
    let step: ProcedureStep = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(step.id.as_deref(), Some("deploy"));
    assert_eq!(step.depends_on, vec!["build", "test"]);
    assert_eq!(step.on_failure, FailureAction::Retry);
    assert_eq!(step.retry.as_ref().unwrap().max_retries, 2);
    assert_eq!(step.timeout_secs, Some(300));
    assert!(step.needs_approval);

    // Legacy step without any DAG fields parses with defaults.
    let legacy: ProcedureStep =
        serde_yaml_ng::from_str("description: run tests\ntool: Bash\n").unwrap();
    assert!(legacy.id.is_none());
    assert!(legacy.depends_on.is_empty());
    assert_eq!(legacy.on_failure, FailureAction::Abort);
    assert!(!legacy.needs_approval);
}

#[test]
fn procedure_step_parses_delegate_to() {
    let yaml = "description: hand off to qa\ndelegate_to: qa\n";
    let s: ProcedureStep = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(s.delegate_to.as_deref(), Some("qa"));
    // Absent → None (every existing skill.yaml still parses).
    let s2: ProcedureStep = serde_yaml_ng::from_str("description: local step\n").unwrap();
    assert_eq!(s2.delegate_to, None);
}

#[test]
fn variable_accepts_legacy_default_value_alias() {
    // Legacy workflow YAML used `default_value`; the unified type aliases it.
    let v: Variable =
        serde_yaml_ng::from_str("name: app\ntype: string\nrequired: true\ndefault_value: my-api\n")
            .unwrap();
    assert_eq!(v.default.as_deref(), Some("my-api"));
    assert_eq!(v.var_type, VarType::String);

    // Modern form `default:` parses too, and choices default empty.
    let v2: Variable = serde_yaml_ng::from_str("name: env\ntype: string\ndefault: prod\n").unwrap();
    assert_eq!(v2.default.as_deref(), Some("prod"));
    assert!(v2.choices.is_empty());
}

#[test]
fn variable_all_vartypes_parse() {
    for t in ["string", "path", "url", "number", "bool", "array"] {
        let v: Variable = serde_yaml_ng::from_str(&format!("name: x\ntype: {t}\n")).unwrap();
        assert_eq!(v.var_type.to_string(), t);
    }
}

#[test]
fn full_manifest_roundtrips() {
    let yaml = r#"
name: research-prices
version: 1.0.0
publisher: human:david
description: Search product prices
category: workflow
hosts: [mur-agent]
content:
  abstract: Searches product prices.
  procedure:
    variables:
      - name: product_name
        type: string
        required: true
    steps:
      - description: Navigate
        tool: browser.navigate
triggers:
  - type: command
    pattern: /research-prices
priority: normal
"#;
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(m.name, "research-prices");
    assert_eq!(m.category, Category::Workflow);
    assert_eq!(m.content.mode(), Some(ContentMode::Workflow));
    let back = serde_yaml_ng::to_string(&m).unwrap();
    let m2: SkillManifest = serde_yaml_ng::from_str(&back).unwrap();
    assert_eq!(m2.name, m.name);
}

#[test]
fn context_mode_detected() {
    let c = Content {
        r#abstract: "a".into(),
        context: Some("ctx".into()),
        procedure: None,
        command: None,
        note: None,
    };
    assert_eq!(c.mode(), Some(ContentMode::Context));
}

#[test]
fn empty_content_returns_no_mode() {
    let c = Content {
        r#abstract: "a".into(),
        context: None,
        procedure: None,
        command: None,
        note: None,
    };
    assert_eq!(c.mode(), None);
}

#[test]
fn mode_returns_note_when_only_note_populated() {
    let c = Content {
        r#abstract: "a".into(),
        context: None,
        procedure: None,
        command: None,
        note: Some("# body".into()),
    };
    assert_eq!(c.mode(), Some(ContentMode::Note));
}

#[test]
fn mode_returns_none_when_note_and_context_both_populated() {
    let c = Content {
        r#abstract: "a".into(),
        context: Some("ctx".into()),
        procedure: None,
        command: None,
        note: Some("# body".into()),
    };
    assert_eq!(c.mode(), None);
}

#[test]
fn skill_without_evolution_log_defaults_to_empty() {
    // YAML without evolution_log field must parse and default to vec![].
    let yaml = r#"
name: no-evol
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: test
"#;
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    assert!(m.evolution_log.is_empty());
}

#[test]
fn skill_with_evolution_log_roundtrips() {
    let yaml = r#"
name: with-evol
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: test
evolution_log:
  - version: "0.1.0"
    generation: 0
    source: "human:test"
    changes: "Initial"
    timestamp: "2026-01-01T00:00:00Z"
"#;
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(m.evolution_log.len(), 1);
    assert_eq!(m.evolution_log[0].version, "0.1.0");
    // Round-trip.
    let back = serde_yaml_ng::to_string(&m).unwrap();
    let m2: SkillManifest = serde_yaml_ng::from_str(&back).unwrap();
    assert_eq!(m2.evolution_log.len(), 1);
    assert_eq!(m2.evolution_log[0].generation, 0);
}

#[test]
fn exact_keyword_returns_pattern_for_keyword_triggers() {
    let t = Trigger {
        kind: TriggerKind::Keyword,
        pattern: Some("search".into()),
    };
    assert_eq!(t.exact_keyword(), Some("search"));
}

#[test]
fn exact_keyword_returns_none_for_non_keyword_triggers() {
    let t = Trigger {
        kind: TriggerKind::Command,
        pattern: Some("run".into()),
    };
    assert_eq!(t.exact_keyword(), None);

    let t = Trigger {
        kind: TriggerKind::SessionStart,
        pattern: None,
    };
    assert_eq!(t.exact_keyword(), None);

    let t = Trigger {
        kind: TriggerKind::Manual,
        pattern: None,
    };
    assert_eq!(t.exact_keyword(), None);
}

#[test]
fn exact_keyword_returns_none_when_pattern_is_none() {
    let t = Trigger {
        kind: TriggerKind::Keyword,
        pattern: None,
    };
    assert_eq!(t.exact_keyword(), None);
}

#[test]
fn skill_scope_serde_and_default() {
    // Default is User.
    assert_eq!(SkillScope::default(), SkillScope::User);
    assert!(SkillScope::User.is_user());
    assert!(!SkillScope::Project.is_user());
    assert!(!SkillScope::Fleet.is_user());

    // Serde: lowercase in YAML.
    let yaml = r#"
name: scoped-skill
version: 0.1.0
publisher: human:test
description: test
category: workflow
scope: fleet
fleet: prod
project: null
content:
  abstract: test
"#;
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(m.scope, SkillScope::Fleet);
    assert_eq!(m.fleet, Some("prod".into()));
    assert_eq!(m.project, None);

    // Round-trip preserves scope.
    let back = serde_yaml_ng::to_string(&m).unwrap();
    let m2: SkillManifest = serde_yaml_ng::from_str(&back).unwrap();
    assert_eq!(m2.scope, SkillScope::Fleet);
    assert_eq!(m2.fleet, Some("prod".into()));

    // Missing scope defaults to User.
    let yaml_no_scope = r#"
name: default-scope
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: test
"#;
    let m3: SkillManifest = serde_yaml_ng::from_str(yaml_no_scope).unwrap();
    assert_eq!(m3.scope, SkillScope::User);
    assert!(m3.fleet.is_none());
    assert!(m3.project.is_none());
}

#[test]
fn scope_visible_matrix() {
    // user + enterprise always visible
    assert!(scope_visible(
        SkillScope::User,
        None,
        None,
        None,
        None,
        None,
        None
    ));
    assert!(scope_visible(
        SkillScope::Enterprise,
        None,
        None,
        None,
        None,
        None,
        None
    ));
    // fleet skill visible only when active fleet matches
    assert!(scope_visible(
        SkillScope::Fleet,
        Some("dev"),
        None,
        None,
        Some("dev"),
        None,
        None
    ));
    assert!(!scope_visible(
        SkillScope::Fleet,
        Some("dev"),
        None,
        None,
        Some("ops"),
        None,
        None
    ));
    assert!(!scope_visible(
        SkillScope::Fleet,
        Some("dev"),
        None,
        None,
        None,
        None,
        None
    ));
    // project skill visible only when active project matches
    assert!(scope_visible(
        SkillScope::Project,
        None,
        Some("/p"),
        None,
        None,
        Some("/p"),
        None
    ));
    assert!(!scope_visible(
        SkillScope::Project,
        None,
        Some("/p"),
        None,
        None,
        Some("/q"),
        None
    ));
}

#[test]
fn team_scope_visibility() {
    // matches when active_team == skill_team
    assert!(scope_visible(
        SkillScope::Team,
        None,
        None,
        Some("org-xyz"),
        None,
        None,
        Some("org-xyz"),
    ));
    // mismatch → false
    assert!(!scope_visible(
        SkillScope::Team,
        None,
        None,
        Some("org-abc"),
        None,
        None,
        Some("org-xyz"),
    ));
    // no active_team → fail-closed
    assert!(!scope_visible(
        SkillScope::Team,
        None,
        None,
        Some("org-xyz"),
        None,
        None,
        None,
    ));
    // no skill_team selector → never injects (None == None guard)
    assert!(!scope_visible(
        SkillScope::Team,
        None,
        None,
        None,
        None,
        None,
        Some("org-xyz"),
    ));
}

#[test]
fn governance_ref_roundtrip() {
    let yaml = "name: t\nversion: 1.0.0\npublisher: human:test\ndescription: t\ncategory: workflow\ncontent:\n  abstract: t\ngovernance:\n  org_id: org-1\n  constitution_hash: abc\n";
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    let g = m.governance.unwrap();
    assert_eq!(g.org_id, "org-1");
    assert_eq!(g.constitution_hash, "abc");
}

#[test]
fn governance_ref_absent_is_none() {
    let m: SkillManifest = serde_yaml_ng::from_str("name: t\nversion: 1.0.0\npublisher: human:test\ndescription: t\ncategory: workflow\ncontent:\n  abstract: t\n").unwrap();
    assert!(m.governance.is_none());
}

#[test]
fn team_field_roundtrip() {
    let yaml = "name: t\nversion: 1.0.0\npublisher: human:test\ndescription: t\ncategory: workflow\ncontent:\n  abstract: t\nscope: team\nteam: org-1\n";
    let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(m.scope, SkillScope::Team);
    assert_eq!(m.team.as_deref(), Some("org-1"));
}
