//! Agent Wizard engine: builds a specialized agent from a role, with a human
//! review gate before anything is written. Drivers (CLI, Hub) share this engine.
pub mod apply;
pub mod catalog;
pub mod draft;
pub mod entitlements;
pub mod stages;

pub use draft::{
    EntitlementPlan, Progress, PromptDraft, RiskLevel, RoleSpec, SkillDraft, Stage, WizardDraft,
    WizardOutcome,
};
