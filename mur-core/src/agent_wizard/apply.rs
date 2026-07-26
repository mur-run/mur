//! Apply an approved draft to disk: create agent, write prompt, attach skills, set perms, start.
use anyhow::Result;
use mur_common::agent::ToolPolicy;

use crate::agent_wizard::draft::{WizardDraft, WizardOutcome};

/// Validate every skill draft by parsing it and running the skill validator.
/// Returns the list of (skill name, error) for any that fail.
pub fn validate_drafts(draft: &WizardDraft) -> Vec<(String, String)> {
    draft
        .skills
        .iter()
        .filter_map(|s| {
            let err = match mur_common::skill::parse_canonical(&s.yaml) {
                Err(e) => Some(e.to_string()),
                Ok(m) => mur_common::skill::validate(&m).err().map(|e| e.to_string()),
            };
            err.map(|e| (s.name.clone(), e))
        })
        .collect()
}

/// Set `model_ref` in the agent's profile.yaml.
fn set_model_ref(name: &str, model_ref: &str) -> Result<()> {
    let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(name)?;
    profile.model_ref = Some(model_ref.to_string());
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    Ok(())
}

/// Stamp the coarse `role` label (from the chosen catalog role) into profile.yaml.
fn set_role(name: &str, role: &str) -> Result<()> {
    let role = role.trim();
    if role.is_empty() {
        return Ok(());
    }
    let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(name)?;
    profile.role = Some(role.to_string());
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    Ok(())
}

/// Write the approved draft to disk and start the agent.
/// Assumes the gate has already approved the draft.
pub fn apply_draft(draft: &WizardDraft) -> Result<WizardOutcome> {
    let name = &draft.role.name;

    // 1. Create the agent profile (provider via anthropic/model shorthand).
    crate::cmd::agent::cmd_create(
        name,
        true, // no_interactive
        Some(draft.role.display_name.clone()),
        Some("anthropic/claude-sonnet-5".to_string()),
        None, // provider parsed from "anthropic/model" shorthand in cmd_create
    )?;

    // 2. Ensure model_ref resolves under launchd.
    set_model_ref(name, &draft.model_ref)?;

    // 2b. Stamp the coarse role label from the chosen catalog role.
    set_role(name, &draft.role.display_name)?;

    // 3. System prompt.
    crate::cmd::agent::cmd_prompt_set(name, Some(&draft.prompt.markdown), None)?;

    // 4. Entitlements — call cmd_perm_* directly (same as agent_admin::perm does).
    let e = &draft.entitlements;
    for p in &e.allow_read {
        crate::cmd::agent::cmd_perm_allow_read(name, p)?;
    }
    for p in &e.allow_write {
        crate::cmd::agent::cmd_perm_allow_write(name, p)?;
    }
    for b in &e.allow_spawn {
        crate::cmd::agent::cmd_perm_allow_spawn(name, b)?;
    }
    for h in &e.allow_host {
        crate::cmd::agent::cmd_perm_allow_host(name, h)?;
    }
    for d in &e.deny_path {
        crate::cmd::agent::cmd_perm_deny_path(name, d)?;
    }
    for t in &e.tool_allow {
        crate::cmd::agent::cmd_perm_set_tool(name, ToolPolicy::Allow, t)?;
    }

    // 5. Attach skills (write each draft to a temp file, then call cmd_skill_add).
    for s in &draft.skills {
        let tmp = std::env::temp_dir().join(format!("mur-wizard-{}-{}.yaml", name, s.name));
        std::fs::write(&tmp, &s.yaml)?;
        let result = crate::cmd::agent::cmd_skill_add(name, tmp.to_str().unwrap());
        let _ = std::fs::remove_file(&tmp);
        result?;
    }

    // 6. Install + start the service.
    crate::cmd::agent::cmd_install_service(name, false)?;

    Ok(WizardOutcome {
        agent_name: name.clone(),
        created: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_wizard::draft::*;

    #[test]
    fn invalid_skill_yaml_is_reported() {
        let draft = WizardDraft {
            role: RoleSpec {
                name: "x".into(),
                display_name: "X".into(),
                charter: "c".into(),
                risk: RiskLevel::Low,
                preset_id: None,
            },
            skills: vec![SkillDraft {
                name: "bad".into(),
                yaml: "not: a valid skill".into(),
            }],
            prompt: PromptDraft {
                markdown: "p".into(),
            },
            entitlements: EntitlementPlan::default(),
            model_ref: "claude_sonnet".into(),
        };
        let errs = validate_drafts(&draft);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].0, "bad");
    }

    /// Regression guard: the deterministic stub skills produced by `build_draft`
    /// must themselves pass the skill validator, or the wizard would abort before
    /// the review gate on its own generated output.
    #[test]
    fn stub_generated_drafts_pass_validation() {
        use crate::agent_wizard::catalog::RoleManifest;
        use crate::agent_wizard::stages::{WizardHooks, build_draft};

        struct NoHooks;
        impl WizardHooks for NoHooks {}

        let m = RoleManifest {
            id: "pm".into(),
            display_name: "PM".into(),
            charter: "Turns intent into buildable, testable work.".into(),
            risk: RiskLevel::Low,
            skill_topics: vec!["product-spec-and-prd-writing".into()],
            category: "product".into(),
        };
        let draft = build_draft(&m, "/repo", "claude_sonnet", &mut NoHooks);
        let errs = validate_drafts(&draft);
        assert!(
            errs.is_empty(),
            "stub skills must pass the validator, got: {errs:?}"
        );
    }
}
