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
    /// Spec 2026-09-07 §1.2: the same derivation `mur agent perm list-paths`
    /// prints. Read-only in P1.
    pub permissions: mur_core::cmd::agent::perm_view::PermissionsView,
    // Model binding (registry ref takes precedence over the inline config)
    pub model_ref: Option<String>,
    pub model_provider: String,
    pub model_name: String,
    // Reasoning effort. `effort_levels` is what THIS agent's model accepts —
    // never a frontend constant, because the set is a property of the model
    // (deepseek-v4 has no `medium`, pre-4.7 Claude no `xhigh`).
    /// The level in force, already narrowed to what this model accepts.
    pub effort: Option<String>,
    /// What the profile actually holds. Differs from `effort` when the stored
    /// level is one this model has no step for — the agent was set on another
    /// model, or by the CLI. The UI must SAY so: the cards only ever offer
    /// levels this model takes, so a user click can never produce this state,
    /// and silently showing the narrowed card would hide that switching the
    /// model back restores the stored value.
    pub effort_stored: Option<String>,
    pub effort_levels: Vec<String>,
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
    /// `subscription` / `usage_billed` / `local`; `None` = unknown, which the
    /// UI renders as Unknown — never as free.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing: Option<String>,
    /// `Some(false)` marks an id typed by hand when catalog discovery failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_verified: Option<bool>,
}

fn model_option_view(ref_name: String, entry: mur_common::model::ModelEntry) -> ModelOptionView {
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
        billing: entry.billing.map(|b| {
            // The serde name of the enum, so Rust and TS agree on the wire.
            serde_json::to_value(b)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        }),
        catalog_verified: entry.catalog_verified,
    }
}

#[tauri::command]
pub fn list_models() -> Result<Vec<ModelOptionView>, String> {
    let path = mur_common::model::ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let reg = mur_common::model::ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    Ok(reg
        .models
        .into_iter()
        .map(|(ref_name, entry)| model_option_view(ref_name, entry))
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
    /// Reasoning effort level. Empty string clears it back to the provider
    /// default; a level name sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Empty string clears the role; non-empty sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// The raw model id this agent actually dials, for keying
/// [`mur_common::llm::effort_shape`].
///
/// `model_ref` wins over the inline `model:` block. That block is legacy data
/// kept as a live read path for agents with no ref, and `AgentDetail.model_name`
/// is filled from it — so keying the effort table on `model_name` would offer
/// the levels of whatever model the profile used to point at. Resolution goes
/// through the registry, exactly as the murmur TUI does.
///
/// Best-effort: an unreadable registry or a dangling ref falls back to the
/// inline name rather than failing the whole detail fetch.
fn resolved_model_id(profile: &mur_common::AgentProfile) -> String {
    profile
        .model_ref
        .as_ref()
        .and_then(|r| {
            mur_common::model::ModelRegistry::default_path()
                .and_then(|p| mur_common::model::ModelRegistry::load_from(&p))
                .ok()
                .and_then(|reg| reg.models.get(r).map(|e| e.model.clone()))
        })
        .unwrap_or_else(|| profile.model.name.clone())
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
    // The seal lives in `running.lock`; with no agent home (tests) there is
    // none, which the view reports as NotRunning — nothing enforced.
    let lock: Option<mur_common::LockFile> = agent_home
        .map(|h| h.join("running.lock"))
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok());
    let permissions = mur_core::cmd::agent::perm_view::permissions_view(&profile, lock.as_ref());

    // Resolved once: `resolved_model_id` reads `models.yaml`, and this runs on
    // every detail fetch and at the end of every update.
    let model_id = resolved_model_id(&profile);
    let effective_effort_str = mur_common::llm::effective_effort(None, profile.effort, &model_id)
        .0
        .map(|e| e.as_str().to_string());
    let effort_levels_str: Vec<String> = mur_common::llm::effort_shape(&model_id)
        .levels()
        .iter()
        .map(|e| e.as_str().to_string())
        .collect();

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
        permissions,
        model_ref: profile.model_ref.clone(),
        model_provider: profile.model.provider.clone(),
        model_name: profile.model.name.clone(),
        effort: effective_effort_str,
        effort_stored: profile.effort.map(|e| e.as_str().to_string()),
        effort_levels: effort_levels_str,
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

    // Apply effort. An empty string clears it; anything else must name a level
    // MUR knows, or the write is refused rather than silently dropped.
    if let Some(raw) = patch.effort {
        profile.effort = if raw.is_empty() {
            None
        } else {
            Some(raw.parse::<mur_common::llm::Effort>()?)
        };
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
    use super::{SkillRefStatusView, build_agent_detail, skill_ref_view};

    /// The Hub must offer the levels THIS agent's model accepts, never a fixed
    /// list. A frontend constant would show `medium` on deepseek-v4 (which has
    /// low/high/max only) and `xhigh` on pre-4.7 Claude (a 400).
    ///
    /// Uses the inline `model:` fallback so the assertion does not depend on
    /// whatever `~/.mur/models.yaml` happens to hold on this machine.
    #[test]
    fn effort_levels_come_from_the_agents_own_model() {
        let mut p = mur_common::AgentProfile::default_for_tests();
        p.model.name = "deepseek-v4-pro".into();
        p.model_ref = None;
        p.effort = Some(mur_common::llm::Effort::Xhigh);

        let d = build_agent_detail(p, None);
        assert_eq!(d.effort_levels, vec!["low", "high", "max"]);
        // Xhigh is not on this model's scale, so the reported value is the
        // nearest level below it — not the stale stored one.
        assert_eq!(d.effort.as_deref(), Some("high"));
        // …and the stored value stays visible, so the UI can say the two
        // differ instead of silently showing the narrowed card.
        assert_eq!(d.effort_stored.as_deref(), Some("xhigh"));
    }

    /// Spec 2026-09-07 §1.4: entitlements reach the DTO through the shared
    /// derivation, and with no agent home the state is NotRunning.
    #[test]
    fn entitlements_project_into_the_detail() {
        use mur_core::cmd::agent::perm_view::{Enforcement, GrantStatus};
        let mut p = mur_common::AgentProfile::default_for_tests();
        p.entitlements.filesystem.write = vec!["/tmp/x".into()];
        p.entitlements.network.outbound.allow_hosts = vec!["api.example.com".into()];
        p.entitlements.tools.push(mur_common::agent::ToolRule {
            pattern: "bash".into(),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
        });

        let d = build_agent_detail(p, None);
        assert_eq!(d.permissions.enforcement, Enforcement::NotRunning);
        assert_eq!(d.permissions.filesystem.write[0].raw, "/tmp/x");
        assert_eq!(
            d.permissions.filesystem.write[0].status,
            GrantStatus::Unverified
        );
        assert_eq!(
            d.permissions.runtime_outbound.allow_hosts,
            vec!["api.example.com"]
        );
        assert_eq!(d.permissions.tools.len(), 1);
        assert_eq!(d.permissions.tools[0].pattern, "bash");
    }

    /// A model with no reasoning control offers nothing and reports nothing,
    /// so the Hub can hide the group instead of showing a dead control.
    #[test]
    fn a_model_without_reasoning_control_offers_no_levels() {
        let mut p = mur_common::AgentProfile::default_for_tests();
        p.model.name = "gpt-4o".into();
        p.model_ref = None;
        p.effort = Some(mur_common::llm::Effort::Max);

        let d = build_agent_detail(p, None);
        assert!(d.effort_levels.is_empty());
        assert_eq!(d.effort, None);
    }

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
mod model_option_view_tests {
    use super::*;
    use mur_common::model::{BillingMode, ModelEntry};

    #[test]
    fn billing_metadata_is_carried_and_absent_when_unknown() {
        let old = model_option_view(
            "gpt".into(),
            ModelEntry {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                ..Default::default()
            },
        );
        assert_eq!(old.billing, None);
        assert_eq!(old.catalog_verified, None);
        let json = serde_json::to_value(&old).unwrap();
        assert!(json.get("billing").is_none(), "{json}");
        assert!(json.get("catalog_verified").is_none(), "{json}");

        let new = model_option_view(
            "chatgpt_sol".into(),
            ModelEntry {
                provider: "codex".into(),
                model: "gpt-5.6-sol".into(),
                billing: Some(BillingMode::Subscription),
                catalog_verified: Some(false),
                ..Default::default()
            },
        );
        let json = serde_json::to_value(&new).unwrap();
        assert_eq!(json["billing"], "subscription");
        assert_eq!(json["catalog_verified"], false);
        for (mode, wire) in [
            (BillingMode::UsageBilled, "usage_billed"),
            (BillingMode::Local, "local"),
        ] {
            let v = model_option_view(
                "x".into(),
                ModelEntry {
                    billing: Some(mode),
                    ..Default::default()
                },
            );
            assert_eq!(v.billing.as_deref(), Some(wire));
        }
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
