/// Every dev-discipline keyword trigger must be a valid regex — the
/// runtime trigger matcher compiles them with `regex::Regex::new`.
#[test]
fn dev_skill_keyword_triggers_compile() {
    let yamls: &[&str] = &[
        include_str!("../../skills/mur_dev.yaml"),
        include_str!("../../skills/mur_grilling.yaml"),
        include_str!("../../skills/mur_brainstorm.yaml"),
        include_str!("../../skills/mur_domain_modeling.yaml"),
        include_str!("../../skills/mur_writing_plans.yaml"),
        include_str!("../../skills/mur_tickets.yaml"),
        include_str!("../../skills/mur_executing_plans.yaml"),
        include_str!("../../skills/mur_delegate_dev.yaml"),
        include_str!("../../skills/mur_worktree.yaml"),
        include_str!("../../skills/mur_tdd.yaml"),
        include_str!("../../skills/mur_debugging.yaml"),
        include_str!("../../skills/mur_code_review.yaml"),
        include_str!("../../skills/mur_receiving_review.yaml"),
        include_str!("../../skills/mur_verification.yaml"),
        include_str!("../../skills/mur_finishing_branch.yaml"),
        include_str!("../../skills/mur_merge_conflicts.yaml"),
        include_str!("../../skills/mur_skill_authoring.yaml"),
    ];
    for y in yamls {
        let m = mur_common::skill::parse_canonical(y).expect("parse");
        for t in &m.triggers {
            if let Some(p) = &t.pattern {
                regex::Regex::new(p)
                    .unwrap_or_else(|e| panic!("{}: trigger regex fails to compile: {e}", m.name));
            }
        }
    }
}
