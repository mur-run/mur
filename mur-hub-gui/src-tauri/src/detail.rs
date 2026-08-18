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
    // Add-ons tab
    pub addons: Vec<InstalledAddonView>,
    // MCP tab
    pub mcp_servers: Vec<McpServerView>,
    // Permissions tab
    pub capabilities: Vec<String>,
    // Model binding (registry ref takes precedence over the inline config)
    pub model_ref: Option<String>,
    pub model_provider: String,
    pub model_name: String,
    // Role (coarse grouping label; editable)
    pub role: Option<String>,
    // Read-only metadata
    pub display_name: String,
    pub agent_name: String,
}

/// One selectable entry from `~/.mur/models.yaml` for the model picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelOptionView {
    pub ref_name: String,
    pub provider: String,
    /// Who makes the model, when that differs from the protocol in
    /// `provider`. The Library groups by this so OpenAI-compatible vendors
    /// (DeepSeek, Groq, a local MLX server) do not all collapse into one
    /// "OpenAI" heading.
    pub vendor: Option<String>,
    pub model: String,
    pub tier: Option<String>,
    pub input_cost: Option<f64>,
    pub output_cost: Option<f64>,
    pub context_window: Option<u64>,
    pub capabilities: Vec<String>,
}

#[tauri::command]
pub fn list_models() -> Result<Vec<ModelOptionView>, String> {
    let path = mur_common::model::ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let reg = mur_common::model::ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    Ok(reg
        .models
        .into_iter()
        .map(|(ref_name, entry)| {
            let (input_cost, output_cost) = entry.effective_costs();
            // Resolve before moving the fields out of `entry`.
            let vendor = entry
                .vendor_candidates()
                .into_iter()
                .next()
                .filter(|v| *v != entry.provider);
            ModelOptionView {
                ref_name,
                vendor,
                provider: entry.provider,
                model: entry.model,
                tier: entry.tier.map(|t| format!("{t:?}").to_lowercase()),
                input_cost,
                output_cost,
                context_window: entry.context_window,
                capabilities: entry.capabilities,
            }
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
    /// Whether the backing file actually parses + validates as a skill
    /// manifest (canonical YAML or markdown). Legacy `.md` paths that no
    /// longer parse are surfaced as `false` so the UI can flag them as dead.
    pub loadable: bool,
    /// Why a skill is (or isn't) loadable, so the UI can distinguish a ref
    /// whose file was never installed from a file that no longer parses
    /// (#717).
    pub status: SkillRefStatusView,
}

/// UI-facing skill-ref status. `Missing` = the resolved manifest file does
/// not exist (profile.yaml references a skill that was never installed);
/// `Malformed` = the file exists but no longer parses/validates; `Corrupt` =
/// the ref itself is not a usable path (several refs concatenated into one
/// entry), so the skills it names may be installed and "install it" is the
/// wrong advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillRefStatusView {
    Ok,
    Missing,
    Malformed,
    Corrupt,
}

/// Classify a legacy per-agent skill path via the shared resolver the runtime
/// loader uses (`mur_common::skill::loader::skill_ref_status`): resolves
/// `rel_path` under the agent home (`skills/<name>` dirs resolve to their
/// `skill.yaml`), then parses + validates the manifest.
fn skill_ref_view(agent_home: &std::path::Path, rel_path: &str) -> SkillRefStatusView {
    use mur_common::skill::loader::SkillRefStatus;
    match mur_common::skill::loader::skill_ref_status(agent_home, rel_path) {
        SkillRefStatus::Loadable => SkillRefStatusView::Ok,
        SkillRefStatus::Missing { .. } => SkillRefStatusView::Missing,
        SkillRefStatus::Malformed { .. } => SkillRefStatusView::Malformed,
        SkillRefStatus::CorruptRef { .. } => SkillRefStatusView::Corrupt,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledAddonView {
    pub id: String,
    pub source: String,
    pub enabled: bool,
    pub skills: Vec<String>,
    pub mcp: Vec<String>,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkillView {
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: String,
    pub enabled: bool,
    pub addon_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub enabled: bool,
    pub addon_id: Option<String>,
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
    /// Source photo for photo-based (polaroid) presets. Must exist on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_image_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
    /// Empty string clears the role; non-empty sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Extract a profile into the Hub's `AgentDetail` view model.
/// Extracted so tests can build a detail directly from an `AgentProfile`
/// without requiring on-disk access. `agent_home` is used only for the
/// legacy per-agent skill-path loadability check; pass `None` to skip it
/// (all paths will be reported as unloadable, which is fine for tests that
/// don't exercise the legacy `skills` list).
pub fn build_agent_detail(
    profile: mur_common::AgentProfile,
    agent_home: Option<&std::path::Path>,
) -> AgentDetail {
    AgentDetail {
        persona_category: format!("{:?}", profile.persona.category).to_lowercase(),
        persona_description: profile.persona.description.clone(),
        persona_tone: profile.persona.traits.tone.clone(),
        persona_risk: profile.persona.traits.risk.clone(),
        persona_verbosity: profile.persona.traits.verbosity.clone(),
        style_preset: profile.appearance.style_preset.clone(),
        render_status: match profile.appearance.render_status {
            mur_common::agent::RenderStatus::Pending => RenderStatusView::Pending,
            mur_common::agent::RenderStatus::Rendering { done, total } => {
                RenderStatusView::Rendering { done, total }
            }
            mur_common::agent::RenderStatus::Ready => RenderStatusView::Ready,
            mur_common::agent::RenderStatus::Failed { ref reason } => RenderStatusView::Failed {
                reason: reason.clone(),
            },
        },
        behavior_preset: format!("{:?}", profile.appearance.behavior_preset).to_lowercase(),
        skills: profile
            .skills
            .iter()
            .map(|path| {
                // No agent home (test construction) → report Missing, which
                // is also the fail-safe reading: nothing resolvable on disk.
                let status = agent_home
                    .map(|home| skill_ref_view(home, path))
                    .unwrap_or(SkillRefStatusView::Missing);
                SkillView {
                    path: path.clone(),
                    loadable: status == SkillRefStatusView::Ok,
                    status,
                }
            })
            .collect(),
        installed_skills: profile
            .installed_skills
            .iter()
            .map(|s| InstalledSkillView {
                enabled: profile.skill_enabled(&s.name),
                addon_id: profile.group_of(&s.name).map(|g| g.id.clone()),
                name: s.name.clone(),
                version: s.version.clone(),
                description: s.description.clone(),
                category: s.category.clone(),
            })
            .collect(),
        addons: profile
            .addons
            .iter()
            .map(|g| InstalledAddonView {
                id: g.id.clone(),
                source: g.source.clone(),
                enabled: g.enabled,
                skills: g.skills.clone(),
                mcp: g.mcp.clone(),
                commands: g.commands.clone(),
            })
            .collect(),
        mcp_servers: profile
            .mcp_servers
            .iter()
            .map(|m| McpServerView {
                enabled: profile.mcp_enabled(&m.name),
                addon_id: profile.group_of(&m.name).map(|g| g.id.clone()),
                name: m.name.clone(),
                command: m.command.clone(),
                args: m.args.clone(),
            })
            .collect(),
        capabilities: profile.capabilities.clone(),
        model_ref: profile.model_ref.clone(),
        model_provider: profile.model.provider.clone(),
        model_name: profile.model.name.clone(),
        role: profile.role.clone(),
        display_name: profile.display_name.clone(),
        agent_name: profile.name.clone(),
    }
}

#[tauri::command]
pub fn get_agent_detail(name: String) -> Result<AgentDetail, String> {
    let mur_home = crate::mur_home_path();
    let profile_path = mur_home.join("agents").join(&name).join("profile.yaml");
    let bytes = std::fs::read(&profile_path).map_err(|e| format!("read profile: {e}"))?;
    let profile: mur_common::AgentProfile =
        serde_yaml_ng::from_slice(&bytes).map_err(|e| format!("parse profile: {e}"))?;
    let agent_home = mur_home.join("agents").join(&name);
    Ok(build_agent_detail(profile, Some(&agent_home)))
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

    // Apply source-photo patch. Photo-based (polaroid) presets render FROM this
    // image, so a path that isn't there would fail deep inside the render job
    // with nothing pointing back at the cause — reject it here instead.
    if let Some(p) = patch.source_image_path {
        let path = std::path::PathBuf::from(&p);
        if !path.exists() {
            return Err(format!("photo not found: {p}"));
        }
        profile.appearance.source_image_path = Some(path);
        profile.appearance.render_status = mur_common::agent::RenderStatus::Pending;
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

    // Apply role patch (empty string clears it)
    if let Some(r) = patch.role {
        let r = r.trim();
        profile.role = if r.is_empty() {
            None
        } else {
            Some(r.to_string())
        };
    }

    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let yaml = serde_yaml_ng::to_string(&profile).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&profile_path, yaml).map_err(|e| format!("write profile: {e}"))?;

    // Return fresh detail after update
    get_agent_detail(name)
}

#[cfg(test)]
mod tests {
    use super::{SkillRefStatusView, skill_ref_view};

    // Regression: per-agent skill ids are directories (`skills/<name>`) holding
    // a `skill.yaml`. The loadable check must resolve into the dir, not try to
    // read the dir as a file (which flagged every skill "unloadable" in the Hub).
    #[test]
    fn directory_form_skill_is_loadable() {
        let home = tempfile::tempdir().unwrap();
        let skill_dir = home.path().join("skills").join("demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("skill.yaml"),
            "name: demo\nversion: 1.0.0\npublisher: human:test\n\
             description: a demo skill for the loadable check\ncategory: context\n\
             provenance: human\nhosts:\n- mur-agent\n\
             content:\n  abstract: demo abstract\n  context: demo context body\n",
        )
        .unwrap();

        assert_eq!(
            skill_ref_view(home.path(), "skills/demo"),
            SkillRefStatusView::Ok
        );
    }

    // #717: a ref written into profile.yaml without installing the backing
    // files must surface as Missing (file not found), not as a parse failure.
    #[test]
    fn absent_ref_is_missing_not_malformed() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            skill_ref_view(home.path(), "skills/executing-plans"),
            SkillRefStatusView::Missing
        );
    }

    #[test]
    fn garbage_manifest_is_malformed_not_missing() {
        let home = tempfile::tempdir().unwrap();
        let skill_dir = home.path().join("skills").join("broken");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("skill.yaml"), "{{{ not yaml at all").unwrap();
        assert_eq!(
            skill_ref_view(home.path(), "skills/broken"),
            SkillRefStatusView::Malformed
        );
    }
}

#[cfg(test)]
mod addon_detail_tests {
    use super::*;

    #[test]
    fn detail_surfaces_addons_and_group_off_dims_members() {
        let mut p = mur_common::agent::AgentProfile::default_for_tests();
        p.installed_skills.push(mur_common::agent::SkillCardEntry {
            name: "g_skill".into(),
            ..Default::default()
        });
        p.addons.push(mur_common::agent::AddonRef {
            id: "grp".into(),
            source: "claude-local:grp@1.0.0".into(),
            enabled: false,
            skills: vec!["g_skill".into()],
            mcp: vec![],
            commands: vec![],
            content_hash: None,
            fetch_ref: None,
            fetch_plugin: None,
        });

        let detail = build_agent_detail(p, None);
        assert_eq!(detail.addons.len(), 1);
        assert!(!detail.addons[0].enabled);
        let row = detail
            .installed_skills
            .iter()
            .find(|s| s.name == "g_skill")
            .unwrap();
        // group off => member shown disabled, with its addon id for the badge
        assert!(!row.enabled);
        assert_eq!(row.addon_id.as_deref(), Some("grp"));
    }
}
