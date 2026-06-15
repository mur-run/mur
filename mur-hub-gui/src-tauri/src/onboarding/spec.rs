//! Agent Wizard — Specialist-path backend.
//!
//! Task 1 of Plan 4: role catalog DTO + `wizard_spec_catalog` command.
//! Task 2 of Plan 4: `SpecDraftDto` + `EmitHooks` + `wizard_spec_generate`.
//! Remaining commands (approve / cancel) are added in Task 3.

use mur_core::agent_wizard::Progress;
use mur_core::agent_wizard::catalog::RoleManifest;
use mur_core::agent_wizard::draft::WizardDraft;
use mur_core::agent_wizard::stages::WizardHooks;
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

// ─── Task 2 DTOs ──────────────────────────────────────────────────────────

/// One generated skill, serialised for the frontend review panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDto {
    pub name: String,
    /// Full `skill.yaml` content (shown in the review panel editor).
    pub yaml: String,
}

/// The full draft sent to the frontend at the review gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecDraftDto {
    pub role_id: String,
    pub display_name: String,
    pub charter: String,
    pub risk: String,
    pub skills: Vec<SkillDto>,
    pub prompt_markdown: String,
    pub model_ref: String,
    /// Serialised entitlement plan (allow_read / allow_write / …) as a JSON value
    /// so the frontend can render it without knowing the exact Rust shape.
    pub entitlements: serde_json::Value,
}

impl SpecDraftDto {
    pub fn from_draft(d: &WizardDraft) -> Self {
        let skills = d
            .skills
            .iter()
            .map(|s| SkillDto {
                name: s.name.clone(),
                yaml: s.yaml.clone(),
            })
            .collect();
        let entitlements = serde_json::to_value(&d.entitlements).unwrap_or(serde_json::Value::Null);
        Self {
            role_id: d.role.name.clone(),
            display_name: d.role.display_name.clone(),
            charter: d.role.charter.clone(),
            risk: format!("{:?}", d.role.risk).to_lowercase(),
            skills,
            prompt_markdown: d.prompt.markdown.clone(),
            model_ref: d.model_ref.clone(),
            entitlements,
        }
    }
}

// ─── EmitHooks ─────────────────────────────────────────────────────────────

/// `WizardHooks` implementation that forwards progress via a caller-supplied sink.
///
/// The sink is generic so unit tests can capture events without a real `AppHandle`:
///
/// ```ignore
/// let mut log = Vec::new();
/// let mut hooks = EmitHooks::new(|p: &Progress| log.push(p.clone()));
/// ```
///
/// The real `wizard_spec_generate` command passes `move |p| { let _ = app.emit(...); }`.
pub struct EmitHooks<F: FnMut(&Progress)> {
    sink: F,
}

impl<F: FnMut(&Progress)> EmitHooks<F> {
    pub fn new(sink: F) -> Self {
        Self { sink }
    }
}

impl<F: FnMut(&Progress)> WizardHooks for EmitHooks<F> {
    fn on_progress(&mut self, p: &Progress) {
        (self.sink)(p);
    }
    // `review_gate` is defaulted to `Some(draft)` in stages.rs — the Hub flow
    // handles the human gate asynchronously via a separate `wizard_spec_approve`
    // command (Task 3), so we do NOT call `run_wizard` here. We only call
    // `build_wizard_draft`, which never invokes `review_gate`.
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

/// Run the wizard engine for the selected role.
///
/// Emits `wizard-spec-progress` events as each stage completes, then stores
/// the finished `WizardDraft` in `WizardSpecState` so `wizard_spec_approve`
/// (Task 3) can apply it.
///
/// * `role_id`  — catalog role id (e.g. `"pm"`).
/// * `no_llm`  — if `true`, skip the LLM and use deterministic stubs.
#[tauri::command]
pub async fn wizard_spec_generate(
    app: tauri::AppHandle,
    role_id: String,
    no_llm: bool,
) -> Result<SpecDraftDto, String> {
    use mur_core::agent_wizard::{DEFAULT_MODEL_REF, build_wizard_draft};
    use std::sync::Arc;
    use tauri::{Emitter, Manager};

    let home = crate::mur_home_path();

    // Load the catalog and find the requested role.
    let catalog = mur_core::agent_wizard::catalog::load_catalog(&home);
    let manifest = catalog
        .into_iter()
        .find(|m| m.id == role_id)
        .ok_or_else(|| format!("role not found: {role_id}"))?;

    // Optionally build the LLM adapter from the user's config.
    let llm: Option<Arc<dyn mur_core::agent_wizard::llm::WizardLlm>> = if no_llm {
        None
    } else {
        match mur_core::conversations::backend::adapter::build_chat_adapter(
            &home,
            None,
            "agent-wizard",
        ) {
            Ok(adapter) => {
                Some(Arc::new(adapter) as Arc<dyn mur_core::agent_wizard::llm::WizardLlm>)
            }
            Err(e) => {
                tracing::warn!("wizard_spec_generate: no LLM available ({e}); using stubs");
                None
            }
        }
    };

    // Clone AppHandle for the progress sink (the sink closure must be 'static-ish,
    // but we only need it for the duration of `build_wizard_draft`, which is fine).
    let app_emit = app.clone();
    let mut hooks = EmitHooks::new(move |p: &Progress| {
        let _ = app_emit.emit("wizard-spec-progress", p);
    });

    let workspace = home.to_str().unwrap_or("~/.mur").to_string();
    let draft = build_wizard_draft(
        &manifest,
        &workspace,
        DEFAULT_MODEL_REF,
        &llm,
        &[],
        &mut hooks,
    )
    .await;

    let dto = SpecDraftDto::from_draft(&draft);

    // Persist the draft for the approve step (Task 3).
    let state = app.state::<WizardSpecState>();
    *state.0.lock().map_err(|e| e.to_string())? = Some(draft);

    Ok(dto)
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

    /// `EmitHooks` forwards every `on_progress` call to the sink closure.
    #[tokio::test]
    async fn emit_hooks_forwards_progress() {
        use mur_core::agent_wizard::{Stage, build_wizard_draft};

        let manifest = RoleManifest {
            id: "pm".into(),
            display_name: "PM".into(),
            charter: "Own the roadmap".into(),
            risk: RiskLevel::Low,
            skill_topics: vec!["roadmap".into()],
            category: "product".into(),
        };

        let mut log: Vec<Progress> = Vec::new();
        let mut hooks = EmitHooks::new(|p: &Progress| log.push(p.clone()));

        let draft =
            build_wizard_draft(&manifest, "/repo", "claude_sonnet", &None, &[], &mut hooks).await;

        // At minimum: DefineRole, AuthorSkills, DraftPrompt, Entitlements
        assert!(
            log.len() >= 4,
            "expected at least 4 progress events, got {}",
            log.len()
        );
        assert!(
            log.iter().any(|p| p.stage == Stage::DefineRole),
            "missing DefineRole event"
        );
        assert!(
            log.iter().any(|p| p.stage == Stage::AuthorSkills),
            "missing AuthorSkills event"
        );

        // Draft should contain the skill stub for "roadmap".
        assert_eq!(draft.skills.len(), 1);
        assert!(draft.skills[0].yaml.contains("name: roadmap"));
    }

    /// `SpecDraftDto::from_draft` correctly maps all fields.
    #[tokio::test]
    async fn spec_draft_dto_from_draft_maps_correctly() {
        use mur_core::agent_wizard::build_wizard_draft;

        let manifest = RoleManifest {
            id: "qa".into(),
            display_name: "QA Engineer".into(),
            charter: "Catch regressions".into(),
            risk: RiskLevel::Medium,
            skill_topics: vec!["testing".into()],
            category: "engineering".into(),
        };

        let mut hooks = EmitHooks::new(|_: &Progress| {});
        let draft = build_wizard_draft(
            &manifest,
            "/workspace",
            "claude_sonnet",
            &None,
            &[],
            &mut hooks,
        )
        .await;
        let dto = SpecDraftDto::from_draft(&draft);

        assert_eq!(dto.role_id, "qa");
        assert_eq!(dto.display_name, "QA Engineer");
        assert_eq!(dto.risk, "medium");
        assert_eq!(dto.model_ref, "claude_sonnet");
        assert_eq!(dto.skills.len(), 1);
        assert_eq!(dto.skills[0].name, "testing");
        assert!(!dto.prompt_markdown.is_empty());
    }
}
