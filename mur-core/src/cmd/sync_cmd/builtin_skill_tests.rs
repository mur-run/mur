#[test]
fn new_builtin_skills_parse_and_respect_disclosure_budgets() {
    // (name, yaml, expect_on_demand)
    let cases: &[(&str, &str, bool)] = &[
        (
            "mur-fleet-manage",
            include_str!("../../skills/mur_fleet_manage.yaml"),
            false,
        ),
        (
            "mur-fleet-loop",
            include_str!("../../skills/mur_fleet_loop.yaml"),
            true,
        ),
        (
            "mur-fleet-share",
            include_str!("../../skills/mur_fleet_share.yaml"),
            true,
        ),
        (
            "mur-workflow-author",
            include_str!("../../skills/mur_workflow_author.yaml"),
            false,
        ),
        (
            "mur-workflow-hitl",
            include_str!("../../skills/mur_workflow_hitl.yaml"),
            true,
        ),
        (
            "mur-workflow-delegate",
            include_str!("../../skills/mur_workflow_delegate.yaml"),
            true,
        ),
        (
            "mur-deep-research",
            include_str!("../../skills/mur_deep_research.yaml"),
            false,
        ),
        (
            "mur-capability",
            include_str!("../../skills/mur_capability.yaml"),
            false,
        ),
        (
            "mur-official",
            include_str!("../../skills/mur_official.yaml"),
            false,
        ),
        (
            "mur-notes",
            include_str!("../../skills/mur_notes.yaml"),
            false,
        ),
        (
            "mur-chat",
            include_str!("../../skills/mur_chat.yaml"),
            false,
        ),
        (
            "mur-agent-setup",
            include_str!("../../skills/mur_agent_setup.yaml"),
            false,
        ),
        (
            "mur-agent-mcp-wire",
            include_str!("../../skills/mur_agent_mcp_wire.yaml"),
            true,
        ),
        (
            "mur-agent-schedule",
            include_str!("../../skills/mur_agent_schedule.yaml"),
            true,
        ),
        (
            "mur-parallel-exec",
            include_str!("../../skills/mur_parallel_exec.yaml"),
            false,
        ),
        (
            "mur-parallel-tracks",
            include_str!("../../skills/mur_parallel_tracks.yaml"),
            true,
        ),
        (
            "mur-parallel-merge",
            include_str!("../../skills/mur_parallel_merge.yaml"),
            true,
        ),
        (
            "parallel-topology-guide",
            include_str!("../../skills/parallel_topology_guide.yaml"),
            true,
        ),
        (
            "parallel-decompose",
            include_str!("../../skills/parallel_decompose.yaml"),
            false,
        ),
        (
            "parallel-code",
            include_str!("../../skills/parallel_code.yaml"),
            false,
        ),
        (
            "mur-native-tools",
            include_str!("../../skills/mur_native_tools.yaml"),
            false,
        ),
        ("mur-dev", include_str!("../../skills/mur_dev.yaml"), false),
        (
            "mur-grilling",
            include_str!("../../skills/mur_grilling.yaml"),
            true,
        ),
        (
            "mur-brainstorm",
            include_str!("../../skills/mur_brainstorm.yaml"),
            true,
        ),
        (
            "mur-domain-modeling",
            include_str!("../../skills/mur_domain_modeling.yaml"),
            true,
        ),
        (
            "mur-writing-plans",
            include_str!("../../skills/mur_writing_plans.yaml"),
            true,
        ),
        (
            "mur-tickets",
            include_str!("../../skills/mur_tickets.yaml"),
            true,
        ),
        (
            "mur-executing-plans",
            include_str!("../../skills/mur_executing_plans.yaml"),
            true,
        ),
        (
            "mur-delegate-dev",
            include_str!("../../skills/mur_delegate_dev.yaml"),
            true,
        ),
        (
            "mur-worktree",
            include_str!("../../skills/mur_worktree.yaml"),
            true,
        ),
        ("mur-tdd", include_str!("../../skills/mur_tdd.yaml"), true),
        (
            "mur-debugging",
            include_str!("../../skills/mur_debugging.yaml"),
            true,
        ),
        (
            "mur-code-review",
            include_str!("../../skills/mur_code_review.yaml"),
            true,
        ),
        (
            "mur-receiving-review",
            include_str!("../../skills/mur_receiving_review.yaml"),
            true,
        ),
        (
            "mur-verification",
            include_str!("../../skills/mur_verification.yaml"),
            true,
        ),
        (
            "mur-finishing-branch",
            include_str!("../../skills/mur_finishing_branch.yaml"),
            true,
        ),
        (
            "mur-merge-conflicts",
            include_str!("../../skills/mur_merge_conflicts.yaml"),
            true,
        ),
        (
            "mur-skill-authoring",
            include_str!("../../skills/mur_skill_authoring.yaml"),
            true,
        ),
        (
            "mur-project-search",
            include_str!("../../skills/mur_project_search.yaml"),
            true,
        ),
        (
            "mur-search",
            include_str!("../../skills/mur_search.yaml"),
            false,
        ),
    ];
    use mur_common::skill::manifest::Visibility;
    for (name, yaml, on_demand) in cases {
        let m = mur_common::skill::parse_canonical(yaml)
            .unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
        assert_eq!(&m.name, name);
        assert_eq!(
            m.visibility == Visibility::OnDemand,
            *on_demand,
            "{name}: wrong visibility"
        );
        assert!(
            m.description.chars().count() <= 120,
            "{name}: description over 120 chars"
        );
        assert!(
            m.content.r#abstract.split_whitespace().count() <= 50,
            "{name}: abstract over 50 words"
        );
        let body = m
            .content
            .context
            .clone()
            .or_else(|| m.content.note.clone())
            .unwrap_or_default();
        let body_lines = body.lines().count();
        assert!(
            body_lines <= 150,
            "{name}: body {body_lines} lines (budget 150)"
        );
    }
}

#[test]
fn deep_research_skill_teaches_agent_dispatch_and_polling() {
    let m = mur_common::skill::parse_canonical(include_str!("../../skills/mur_deep_research.yaml"))
        .expect("mur-deep-research must parse");
    assert_eq!(m.version.to_string(), "0.2.0");
    let body = m.content.context.unwrap_or_default();
    assert!(body.contains("fleet_run"), "{body}");
    assert!(body.contains("mur_job_status"), "{body}");
    assert!(!body.contains("MUR_RUN_ID"), "{body}");
}

/// Every built-in skill YAML must deserialize into a `SkillManifest`.
///
/// The case list above — and the two other lists in this file — are
/// hand-maintained, so a skill is only covered if someone remembers to add
/// it. `mur_settlement.yaml` shipped a `procedure: []` (an empty sequence
/// where the schema wants a `Procedure` struct) and no test noticed: it was
/// in none of the lists, and its own test asserted on a raw substring
/// rather than parsing. `ensure_mur_skill` still wrote the file, so the
/// only symptom was a WARN during `mur sync` and the skill silently missing
/// from retrieval.
///
/// Reading the directory instead of a literal list is what makes this
/// unmissable: a new skill is covered the moment it lands.
#[test]
fn every_builtin_skill_yaml_parses() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/skills");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("read src/skills") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).expect("read skill yaml");
        mur_common::skill::parse_canonical(&raw)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
        checked += 1;
    }
    assert!(checked > 0, "no built-in skill YAML found in {dir:?}");
}

/// The routing contract IS this skill: delete a rule and the agent
/// silently loses a branch — no parse error, no failing build, just a
/// worse search forever after.
///
/// What this proves: the rules survive an edit and the file still parses.
/// What it does NOT prove: that a model obeys them. That check is the
/// fresh-context routing eval in
/// docs/superpowers/plans/2026-09-14-smart-project-search-routing.md.
#[test]
fn project_search_skill_carries_the_routing_contract() {
    let m =
        mur_common::skill::parse_canonical(include_str!("../../skills/mur_project_search.yaml"))
            .expect("mur-project-search must parse");
    let body = m.content.context.clone().unwrap_or_default();
    for needle in [
        "mur project status",
        "--json",
        "indexing_in_progress",
        "stale_dims",
        "git status --porcelain=v1 -z",
        "changed paths",
        "--all",
        "--project",
    ] {
        assert!(
            body.contains(needle),
            "routing rule missing from mur-project-search: {needle}"
        );
    }
    // The old body claimed the default scope was every indexed project.
    // The code defaults to the current directory's project; a skill that
    // says otherwise teaches agents to misread their own results.
    assert!(
        !body.contains("searches across ALL indexed projects"),
        "stale scope claim still present in mur-project-search"
    );
}
