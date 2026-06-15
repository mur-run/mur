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
use crate::agent_wizard::stages::{WizardHooks, build_draft};

/// Full engine entry point: build draft (stages 1-5) -> validate -> gate -> apply.
pub fn run_wizard(
    manifest: &RoleManifest,
    workspace: &str,
    model_ref: &str,
    hooks: &mut impl WizardHooks,
) -> anyhow::Result<WizardOutcome> {
    let draft = build_draft(manifest, workspace, model_ref, hooks);
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
