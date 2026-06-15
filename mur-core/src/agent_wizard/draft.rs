//! In-memory draft types produced by the wizard before anything is written to disk.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Read-only / advisory agents (e.g. PM). No irreversible actions.
    #[default]
    Low,
    /// Writes code/tests, runs build tools (e.g. QA).
    Medium,
    /// Performs irreversible repo/release ops (e.g. RepoManager). Triggers security eval suites.
    High,
}

/// What the user wants built. Either chosen from a catalog preset or fully custom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleSpec {
    pub name: String,
    pub display_name: String,
    pub charter: String,
    pub risk: RiskLevel,
    /// Catalog preset id this came from, or `None` for a fully custom role.
    pub preset_id: Option<String>,
}

/// One generated skill, not yet installed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillDraft {
    pub name: String,
    /// Full `skill.yaml` content (validated before it can reach the review gate).
    pub yaml: String,
}

/// The DoD system prompt draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptDraft {
    pub markdown: String,
}

/// A least-privilege entitlement plan, expressed as the `agent_admin::perm` calls to make.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntitlementPlan {
    pub allow_read: Vec<String>,
    pub allow_write: Vec<String>,
    pub allow_spawn: Vec<String>,
    pub allow_host: Vec<String>,
    pub deny_path: Vec<String>,
    pub tool_allow: Vec<String>,
}

/// Everything the human reviews at the gate, before any disk write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WizardDraft {
    pub role: RoleSpec,
    pub skills: Vec<SkillDraft>,
    pub prompt: PromptDraft,
    pub entitlements: EntitlementPlan,
    pub model_ref: String,
}

/// Progress event emitted to the driver as stages run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub stage: Stage,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    DefineRole,
    Research,
    AuthorSkills,
    DraftPrompt,
    Entitlements,
    ReviewGate,
    Create,
    Eval,
}

/// Result of a completed wizard run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WizardOutcome {
    pub agent_name: String,
    pub created: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_level_defaults_to_low() {
        assert_eq!(RiskLevel::default(), RiskLevel::Low);
    }

    #[test]
    fn role_spec_roundtrips_through_yaml() {
        let r = RoleSpec {
            name: "pm".into(),
            display_name: "PM".into(),
            charter: "Turns intent into buildable, testable work.".into(),
            risk: RiskLevel::Low,
            preset_id: Some("pm".into()),
        };
        let y = serde_yaml_ng::to_string(&r).unwrap();
        let back: RoleSpec = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(r, back);
    }
}
