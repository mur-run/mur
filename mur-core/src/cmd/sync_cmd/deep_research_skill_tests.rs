/// Loader test (Task 9): the three deep-research fleet skills must parse
/// into `SkillManifest`, be scoped to the `deep-research` fleet (not the
/// default `User` scope), and carry a non-empty procedure — mirrors
/// `builtin_skill_tests::new_builtin_skills_parse_and_respect_disclosure_budgets`
/// above, adapted for `scope: fleet` + procedure-mode content instead of
/// the CLI-tool-hint disclosure budgets those skills use.
#[test]
fn deep_research_skills_parse_scope_fleet_with_nonempty_procedure() {
    let cases: &[(&str, &str)] = &[
        (
            "deep-research-router",
            include_str!("../../skills/deep_research_router.yaml"),
        ),
        (
            "deep-research-worker",
            include_str!("../../skills/deep_research_worker.yaml"),
        ),
        (
            "deep-research-verify",
            include_str!("../../skills/deep_research_verify.yaml"),
        ),
    ];
    use mur_common::skill::manifest::SkillScope;
    for (name, yaml) in cases {
        let m = mur_common::skill::parse_canonical(yaml)
            .unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
        assert_eq!(&m.name, name);
        assert_eq!(m.scope, SkillScope::Fleet, "{name}: scope must be Fleet");
        assert_eq!(
            m.fleet.as_deref(),
            Some("deep-research"),
            "{name}: fleet selector must be deep-research"
        );
        let steps = &m
            .content
            .procedure
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: content.procedure must be Some"))
            .steps;
        assert!(
            !steps.is_empty(),
            "{name}: procedure steps must be non-empty"
        );
        for step in steps {
            assert!(
                !step.description.trim().is_empty(),
                "{name}: every step needs a non-empty description"
            );
        }
    }

    // The retired escalation-ladder skill must not be recreated under
    // either its old aura-* name or a deep-research-* rename.
    for (name, yaml) in cases {
        assert!(
            !yaml.to_lowercase().contains("aura-"),
            "{name}: must not reference the retired aura-* skills"
        );
    }
}

#[test]
fn deep_research_router_emits_own_line_convergence_marker() {
    let yaml = include_str!("../../skills/deep_research_router.yaml");
    let m = mur_common::skill::parse_canonical(yaml).unwrap();
    let steps = m.content.procedure.unwrap().steps;
    let body: String = steps
        .iter()
        .map(|s| s.description.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.lines()
            .any(|l| l.trim() == "RESEARCH_COMPLETE" || l.trim() == "`RESEARCH_COMPLETE`"),
        "router skill must instruct emitting RESEARCH_COMPLETE alone on its own line"
    );
}
