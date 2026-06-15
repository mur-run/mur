//! Agent Wizard — Specialist-path backend.
//!
//! Task 1 of Plan 4: role catalog DTO + `wizard_spec_catalog` command.
//! Remaining commands (generate / approve / cancel) are added in Tasks 2-3.

use mur_core::agent_wizard::catalog::RoleManifest;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

// ─── DTOs ─────────────────────────────────────────────────────────────────

/// Serialisable summary of one role sent to the frontend role-picker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoleChoice {
    pub id: String,
    pub display_name: String,
    pub charter: String,
    /// Lowercase risk string: "low", "medium", "high".
    pub risk: String,
    pub skill_topics: Vec<String>,
    pub category: String,
}

impl From<&RoleManifest> for RoleChoice {
    fn from(m: &RoleManifest) -> Self {
        Self {
            id: m.id.clone(),
            display_name: m.display_name.clone(),
            charter: m.charter.clone(),
            risk: format!("{:?}", m.risk).to_lowercase(),
            skill_topics: m.skill_topics.clone(),
            category: m.category.clone(),
        }
    }
}

// ─── Managed state ─────────────────────────────────────────────────────────

/// Holds the in-progress `WizardDraft` between `wizard_spec_generate` and
/// `wizard_spec_approve`. `None` = no spec wizard session open.
pub struct WizardSpecState(pub Mutex<Option<mur_core::agent_wizard::draft::WizardDraft>>);

impl Default for WizardSpecState {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

// ─── Commands ──────────────────────────────────────────────────────────────

/// Return the full role catalog so the frontend can render the role-picker.
///
/// Uses `mur_home_path()` from the parent module (mirrors the existing
/// onboarding commands) — no new home-resolution logic needed.
#[tauri::command]
pub fn wizard_spec_catalog() -> Result<Vec<RoleChoice>, String> {
    let home = crate::mur_home_path();
    let roles = mur_core::agent_wizard::catalog::load_catalog(&home);
    Ok(roles.iter().map(RoleChoice::from).collect())
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_core::agent_wizard::catalog::RoleManifest;
    use mur_core::agent_wizard::draft::RiskLevel;

    #[test]
    fn role_manifest_maps() {
        let m = RoleManifest {
            id: "pm".into(),
            display_name: "Product Manager".into(),
            charter: "Own the roadmap".into(),
            risk: RiskLevel::Low,
            skill_topics: vec!["roadmap".into()],
            category: "product".into(),
        };
        let c = RoleChoice::from(&m);
        assert_eq!(c.id, "pm");
        assert_eq!(c.display_name, "Product Manager");
        assert_eq!(c.risk, "low");
        assert_eq!(c.category, "product");
        assert_eq!(c.skill_topics, vec!["roadmap"]);
    }

    #[test]
    fn wizard_spec_state_default_is_none() {
        let state = WizardSpecState::default();
        assert!(state.0.lock().unwrap().is_none());
    }
}
