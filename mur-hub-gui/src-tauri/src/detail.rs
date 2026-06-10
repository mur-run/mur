//! Agent detail panel — full-profile read + partial write for the Hub's
//! right-side slide-in panel (Persona / Style / Behavior / Skills / MCP /
//! Permissions / Inbox tabs).

use serde::{Deserialize, Serialize};

/// All detail-panel tab data extracted from one AgentProfile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDetail {
    // Persona tab
    pub persona_category: String,
    pub persona_description: String,
    pub persona_tone: String,
    pub persona_risk: String,
    pub persona_verbosity: String,
    // Style tab
    pub style_preset: String,
    pub render_status: RenderStatusView,
    // Behavior tab
    pub behavior_preset: String,
    // Skills tab
    pub skills: Vec<SkillView>,
    pub installed_skills: Vec<InstalledSkillView>,
    // MCP tab
    pub mcp_servers: Vec<McpServerView>,
    // Permissions tab
    pub capabilities: Vec<String>,
    // Model binding (registry ref takes precedence over the inline config)
    pub model_ref: Option<String>,
    pub model_provider: String,
    pub model_name: String,
    // Read-only metadata
    pub display_name: String,
    pub agent_name: String,
}

/// One selectable entry from `~/.mur/models.yaml` for the model picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelOptionView {
    pub ref_name: String,
    pub provider: String,
    pub model: String,
}

#[tauri::command]
pub fn list_models() -> Result<Vec<ModelOptionView>, String> {
    let path = mur_common::model::ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let reg = mur_common::model::ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    Ok(reg
        .models
        .into_iter()
        .map(|(ref_name, entry)| ModelOptionView {
            ref_name,
            provider: entry.provider,
            model: entry.model,
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RenderStatusView {
    Pending,
    Rendering { done: u8, total: u8 },
    Ready,
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillView {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkillView {
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Partial update to an agent's profile. All fields optional — only set
/// fields are applied.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DetailPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_tone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_risk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_verbosity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
}

#[tauri::command]
pub fn get_agent_detail(name: String) -> Result<AgentDetail, String> {
    let mur_home = crate::mur_home_path();
    let profile_path = mur_home.join("agents").join(&name).join("profile.yaml");
    let bytes = std::fs::read(&profile_path).map_err(|e| format!("read profile: {e}"))?;
    let profile: mur_common::AgentProfile =
        serde_yaml_ng::from_slice(&bytes).map_err(|e| format!("parse profile: {e}"))?;

    Ok(AgentDetail {
        persona_category: format!("{:?}", profile.persona.category).to_lowercase(),
        persona_description: profile.persona.description,
        persona_tone: profile.persona.traits.tone,
        persona_risk: profile.persona.traits.risk,
        persona_verbosity: profile.persona.traits.verbosity,
        style_preset: profile.appearance.style_preset,
        render_status: match profile.appearance.render_status {
            mur_common::agent::RenderStatus::Pending => RenderStatusView::Pending,
            mur_common::agent::RenderStatus::Rendering { done, total } => {
                RenderStatusView::Rendering { done, total }
            }
            mur_common::agent::RenderStatus::Ready => RenderStatusView::Ready,
            mur_common::agent::RenderStatus::Failed { reason } => {
                RenderStatusView::Failed { reason }
            }
        },
        behavior_preset: format!("{:?}", profile.appearance.behavior_preset).to_lowercase(),
        skills: profile
            .skills
            .into_iter()
            .map(|path| SkillView { path })
            .collect(),
        installed_skills: profile
            .installed_skills
            .into_iter()
            .map(|s| InstalledSkillView {
                name: s.name,
                version: s.version,
                description: s.description,
                category: s.category,
            })
            .collect(),
        mcp_servers: profile
            .mcp_servers
            .into_iter()
            .map(|m| McpServerView {
                name: m.name,
                command: m.command,
                args: m.args,
            })
            .collect(),
        capabilities: profile.capabilities,
        model_ref: profile.model_ref,
        model_provider: profile.model.provider,
        model_name: profile.model.name,
        display_name: profile.display_name,
        agent_name: profile.name,
    })
}

#[tauri::command]
pub fn update_agent_detail(name: String, patch: DetailPatch) -> Result<AgentDetail, String> {
    let mur_home = crate::mur_home_path();
    let profile_path = mur_home.join("agents").join(&name).join("profile.yaml");
    let bytes = std::fs::read(&profile_path).map_err(|e| format!("read profile: {e}"))?;
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_slice(&bytes).map_err(|e| format!("parse profile: {e}"))?;

    // Apply persona patches
    if let Some(cat) = patch.persona_category {
        profile.persona.category = match cat.as_str() {
            "research" => mur_common::agent::PersonaCategory::Research,
            "automation" => mur_common::agent::PersonaCategory::Automation,
            "monitor" => mur_common::agent::PersonaCategory::Monitor,
            "notify" => mur_common::agent::PersonaCategory::Notify,
            "commerce" => mur_common::agent::PersonaCategory::Commerce,
            _ => mur_common::agent::PersonaCategory::Custom,
        };
    }
    if let Some(d) = patch.persona_description {
        profile.persona.description = d;
    }
    if let Some(t) = patch.persona_tone {
        profile.persona.traits.tone = t;
    }
    if let Some(r) = patch.persona_risk {
        profile.persona.traits.risk = r;
    }
    if let Some(v) = patch.persona_verbosity {
        profile.persona.traits.verbosity = v;
    }

    // Apply style patch
    if let Some(s) = patch.style_preset {
        if s != profile.appearance.style_preset {
            // The rendered expressions belong to the old style; flag the
            // avatar as needing a re-render so the UI doesn't claim "ready".
            profile.appearance.render_status = mur_common::agent::RenderStatus::Pending;
        }
        profile.appearance.style_preset = s;
    }

    // Apply model patch — the ref must exist in the registry so the agent
    // doesn't end up pointing at a model nobody configured.
    if let Some(r) = patch.model_ref {
        let reg_path =
            mur_common::model::ModelRegistry::default_path().map_err(|e| e.to_string())?;
        let reg =
            mur_common::model::ModelRegistry::load_from(&reg_path).map_err(|e| e.to_string())?;
        if !reg.models.contains_key(&r) {
            return Err(format!("model ref '{r}' not found in ~/.mur/models.yaml"));
        }
        profile.model_ref = Some(r);
    }

    // Apply behavior patch
    if let Some(b) = patch.behavior_preset {
        profile.appearance.behavior_preset = match b.as_str() {
            "quiet" => mur_common::agent::BehaviorPreset::Quiet,
            "lively" => mur_common::agent::BehaviorPreset::Lively,
            _ => mur_common::agent::BehaviorPreset::Normal,
        };
    }

    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let yaml = serde_yaml_ng::to_string(&profile).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&profile_path, yaml).map_err(|e| format!("write profile: {e}"))?;

    // Return fresh detail after update
    get_agent_detail(name)
}
