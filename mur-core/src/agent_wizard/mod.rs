//! Agent Wizard engine: builds a specialized agent from a role, with a human
//! review gate before anything is written. Drivers (CLI, Hub) share this engine.
pub mod apply;
pub mod catalog;
pub mod draft;
pub mod entitlements;
pub mod llm;
pub mod research;
pub mod stages;

pub use draft::{Progress, Stage, WizardOutcome};

use crate::agent_wizard::catalog::RoleManifest;
use crate::agent_wizard::draft::{PromptDraft, RoleSpec, WizardDraft};
use crate::agent_wizard::llm::WizardLlm;
use crate::agent_wizard::research::{ResearchNote, SearchProvider};
use crate::agent_wizard::stages::{WizardHooks, build_draft, template_prompt};
use std::sync::Arc;

/// Assemble the draft (stages 1-5). Uses the LLM for skills+prompt when present,
/// else Plan-1 stubs/templates. Does NOT write anything to disk (unit-testable).
pub async fn build_wizard_draft(
    manifest: &RoleManifest,
    workspace: &str,
    model_ref: &str,
    llm: &Option<Arc<dyn WizardLlm>>,
    notes: &[ResearchNote],
    hooks: &mut impl WizardHooks,
) -> WizardDraft {
    // No LLM -> deterministic Plan-1 stub path.
    let Some(llm) = llm else {
        return build_draft(manifest, workspace, model_ref, hooks);
    };

    let role = RoleSpec {
        name: manifest.id.clone(),
        display_name: manifest.display_name.clone(),
        charter: manifest.charter.clone(),
        risk: manifest.risk,
        preset_id: Some(manifest.id.clone()),
    };
    hooks.on_progress(&Progress {
        stage: Stage::DefineRole,
        message: format!("role {}", role.name),
    });

    let skills =
        match llm::author_skills_llm(llm.as_ref(), &role, &manifest.skill_topics, notes).await {
            Ok(s) => s,
            Err(e) => {
                // Graceful: fall back to stubs on LLM failure, flagged via progress.
                hooks.on_progress(&Progress {
                    stage: Stage::AuthorSkills,
                    message: format!("LLM failed ({e}); using stubs"),
                });
                return build_draft(manifest, workspace, model_ref, hooks);
            }
        };
    hooks.on_progress(&Progress {
        stage: Stage::AuthorSkills,
        message: format!("{} skills", skills.len()),
    });

    let prompt_md = llm::draft_prompt_llm(llm.as_ref(), &role)
        .await
        .unwrap_or_else(|_| template_prompt(&role));
    hooks.on_progress(&Progress {
        stage: Stage::DraftPrompt,
        message: "prompt drafted".into(),
    });

    let entitlements = crate::agent_wizard::entitlements::preset_for(&role, workspace);
    hooks.on_progress(&Progress {
        stage: Stage::Entitlements,
        message: "entitlements scoped".into(),
    });

    WizardDraft {
        role,
        skills,
        prompt: PromptDraft {
            markdown: prompt_md,
        },
        entitlements,
        model_ref: model_ref.to_string(),
    }
}

/// Full entry: build draft -> validate -> gate -> apply.
/// `search` is reserved for Task 4's `SearchProvider` wiring; pass `None` for now.
pub async fn run_wizard(
    manifest: &RoleManifest,
    workspace: &str,
    model_ref: &str,
    llm: Option<Arc<dyn WizardLlm>>,
    _search: Option<Arc<dyn SearchProvider>>,
    hooks: &mut impl WizardHooks,
) -> anyhow::Result<WizardOutcome> {
    let draft = build_wizard_draft(manifest, workspace, model_ref, &llm, &[], hooks).await;
    let errs = apply::validate_drafts(&draft);
    if !errs.is_empty() {
        anyhow::bail!("skill validation failed: {errs:?}");
    }
    hooks.on_progress(&Progress {
        stage: Stage::ReviewGate,
        message: "awaiting human approval".into(),
    });
    let Some(approved) = hooks.review_gate(draft) else {
        return Ok(WizardOutcome {
            agent_name: manifest.id.clone(),
            created: false,
        });
    };
    hooks.on_progress(&Progress {
        stage: Stage::Create,
        message: "creating agent".into(),
    });
    apply::apply_draft(&approved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_wizard::catalog::RoleManifest;
    use crate::agent_wizard::draft::RiskLevel;
    use mur_common::error::LlmError;

    struct GateOnly; // approves without editing, no progress output
    impl WizardHooks for GateOnly {}

    struct MockLlm(String);
    impl mur_common::llm::LlmClient for MockLlm {
        fn complete(
            &self,
            _p: &str,
            _s: Option<&str>,
        ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
            let v = self.0.clone();
            async move { Ok(v) }
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>, LlmError> {
            Ok(vec![])
        }
    }

    fn manifest() -> RoleManifest {
        RoleManifest {
            id: "pm".into(),
            display_name: "PM".into(),
            charter: "c".into(),
            risk: RiskLevel::Low,
            skill_topics: vec!["product-spec".into()],
            category: "product".into(),
        }
    }

    fn skill_yaml() -> String {
        "name: product-spec\nversion: 1.0.0\npublisher: human:pm\n\
description: Spec skill used when writing specs in the repo.\ncategory: context\n\
hosts: [mur-agent]\npriority: normal\ntags: [pm]\ntriggers:\n  - type: session_start\n\
  - type: command\n    pattern: /product-spec\ncontent:\n  abstract: A.\n  context: |\n    # x\n    - Do. *Why: y.*\n"
            .into()
    }

    #[tokio::test]
    async fn llm_present_uses_llm_skills() {
        let llm: Option<Arc<dyn WizardLlm>> = Some(Arc::new(MockLlm(skill_yaml())));
        let draft = build_wizard_draft(
            &manifest(),
            "/repo",
            "claude_sonnet",
            &llm,
            &[],
            &mut GateOnly,
        )
        .await;
        // LLM skill yaml differs from the stub (no "Stub generated" marker):
        assert!(!draft.skills[0].yaml.contains("Stub generated"));
        assert!(draft.skills[0].yaml.starts_with("name: product-spec"));
    }

    #[tokio::test]
    async fn no_llm_falls_back_to_stub() {
        let llm: Option<Arc<dyn WizardLlm>> = None;
        let draft = build_wizard_draft(
            &manifest(),
            "/repo",
            "claude_sonnet",
            &llm,
            &[],
            &mut GateOnly,
        )
        .await;
        assert!(draft.skills[0].yaml.contains("Stub generated"));
    }
}
