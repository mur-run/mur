pub mod first_launch;
pub use first_launch::{check_first_launch, mark_first_launch_done};

use mur_common::agent::{AgentAppearance, BehaviorPreset, RenderStatus};
use mur_common::hub::preset_loader::{default_blob, find_preset};
use mur_common::hub::style_preset::PresetFamily;
use mur_gui_core::image_gen::gemini::GeminiImageGenProvider;
use mur_gui_core::image_gen::mock::MockImageGenProvider;
use mur_gui_core::image_gen::{CancelToken, RenderProgress};
use mur_gui_core::render::RenderJob;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

// ─── Wizard state ──────────────────────────────────────────────────────────

/// Persona category for step 1 (mirrors mur_common::agent::PersonaCategory).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WizardPersona {
    Research,
    Automation,
    Monitor,
    Notify,
    Commerce,
    Custom,
}

/// Step-by-step state accumulated during the wizard flow.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WizardSession {
    /// Current active step (1–6).
    pub step: u8,
    pub persona: Option<WizardPersona>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub style_preset_id: Option<String>,
    pub behavior_preset: Option<BehaviorPreset>,
    /// Absolute path to source photo (step 5, polaroid family only).
    pub source_photo: Option<PathBuf>,
    /// Latest render progress snapshot (step 6).
    pub render_progress: Option<RenderProgressSnapshot>,
    /// Set to true once the render has completed successfully.
    pub render_done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderProgressSnapshot {
    pub total: u32,
    pub done: u32,
    pub failed: u32,
}

impl From<&RenderProgress> for RenderProgressSnapshot {
    fn from(p: &RenderProgress) -> Self {
        Self {
            total: p.total,
            done: p.done,
            failed: p.failed,
        }
    }
}

/// Tauri managed state: one wizard session at a time (None = closed).
pub struct WizardState(pub Mutex<Option<WizardSession>>);

// ─── Step info returned to the frontend ────────────────────────────────────

/// Full wizard state sent to the frontend on every state-changing command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WizardSnapshot {
    pub step: u8,
    pub persona: Option<WizardPersona>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub style_preset_id: Option<String>,
    pub preset_family: Option<String>,
    pub behavior_preset: Option<String>,
    pub needs_photo: bool,
    pub source_photo: Option<String>,
    pub render_progress: Option<RenderProgressSnapshot>,
    pub render_done: bool,
}

impl WizardSnapshot {
    fn from_session(session: &WizardSession, mur_home: &std::path::Path) -> Self {
        let needs_photo = session
            .style_preset_id
            .as_deref()
            .and_then(|id| find_preset(id, &mur_home.join("hub")).ok())
            .map(|p| p.family == PresetFamily::Polaroid && p.llm_image_gen.requires_source_image)
            .unwrap_or(false);

        let preset_family = session
            .style_preset_id
            .as_deref()
            .and_then(|id| find_preset(id, &mur_home.join("hub")).ok())
            .map(|p| format!("{:?}", p.family).to_lowercase());

        Self {
            step: session.step,
            persona: session.persona.clone(),
            name: session.name.clone(),
            description: session.description.clone(),
            style_preset_id: session.style_preset_id.clone(),
            preset_family,
            behavior_preset: session
                .behavior_preset
                .map(|b| format!("{b:?}").to_lowercase()),
            needs_photo,
            source_photo: session
                .source_photo
                .as_ref()
                .and_then(|p| p.to_str().map(String::from)),
            render_progress: session.render_progress.clone(),
            render_done: session.render_done,
        }
    }
}

// ─── Tauri commands ────────────────────────────────────────────────────────

/// Open the wizard and return initial state (step 1).
#[tauri::command]
pub fn wizard_open(state: State<'_, WizardState>) -> WizardSnapshot {
    let session = WizardSession {
        step: 1,
        ..Default::default()
    };
    let snap = WizardSnapshot::from_session(&session, &mur_home_path());
    *state.0.lock().unwrap() = Some(session);
    snap
}

/// Step 1 — set persona category.
#[tauri::command]
pub fn wizard_set_persona(
    persona: WizardPersona,
    state: State<'_, WizardState>,
) -> Result<WizardSnapshot, String> {
    update_session(&state, |s| {
        s.persona = Some(persona);
        s.step = 2;
    })
}

/// Step 2 — set agent name and description.
#[tauri::command]
pub fn wizard_set_name(
    name: String,
    description: String,
    state: State<'_, WizardState>,
) -> Result<WizardSnapshot, String> {
    if name.trim().is_empty() {
        return Err("name must not be empty".into());
    }
    // The name later becomes an agent directory component (wizard_start_render
    // / wizard_finish); validate it here at the single entry point.
    mur_common::agent_name::validate_agent_name(name.trim())
        .map_err(|e| format!("invalid name: {e}"))?;
    update_session(&state, |s| {
        s.name = Some(name.trim().to_string());
        s.description = Some(description.trim().to_string());
        s.step = 3;
    })
}

/// Step 3 — set style preset.
#[tauri::command]
pub fn wizard_set_preset(
    preset_id: String,
    state: State<'_, WizardState>,
) -> Result<WizardSnapshot, String> {
    // Validate that the preset exists.
    let hub_dir = mur_home_path().join("hub");
    find_preset(&preset_id, &hub_dir).map_err(|e| format!("unknown preset: {e}"))?;
    update_session(&state, |s| {
        s.style_preset_id = Some(preset_id);
        s.step = 4;
    })
}

/// Step 4 — set behavior preset.
#[tauri::command]
pub fn wizard_set_behavior(
    behavior: String,
    state: State<'_, WizardState>,
) -> Result<WizardSnapshot, String> {
    let bp = match behavior.as_str() {
        "quiet" => BehaviorPreset::Quiet,
        "normal" => BehaviorPreset::Normal,
        "lively" => BehaviorPreset::Lively,
        other => return Err(format!("unknown behavior preset: {other}")),
    };
    update_session(&state, |s| {
        s.behavior_preset = Some(bp);
        // Skip to step 6 unless polaroid family needs a photo (checked in start_render).
        s.step = 5;
    })
}

/// Step 5 — provide source photo path (polaroid family only).
#[tauri::command]
pub fn wizard_set_photo(
    path: String,
    state: State<'_, WizardState>,
) -> Result<WizardSnapshot, String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("photo not found: {path}"));
    }
    update_session(&state, |s| {
        s.source_photo = Some(p);
        s.step = 6;
    })
}

/// Step 6 — start background expression render.
///
/// Emits `wizard-render-progress` events as each image completes.
/// Emits `wizard-render-done` when the full batch finishes.
#[tauri::command]
pub async fn wizard_start_render(
    app: AppHandle,
    state: State<'_, WizardState>,
) -> Result<(), String> {
    let (preset, agent_dir, cancel) = {
        let guard = state.0.lock().unwrap();
        let session = guard.as_ref().ok_or("no wizard session")?;

        // Resolve preset.
        let hub_dir = mur_home_path().join("hub");
        let preset_id = session.style_preset_id.as_deref().unwrap_or("default-blob");
        let preset = find_preset(preset_id, &hub_dir).unwrap_or_else(|_| default_blob());

        // Agent expressions dir.
        let name = session.name.clone().unwrap_or_else(|| "unnamed".into());
        let agent_dir = mur_home_path().join("agents").join(&name);

        (preset, agent_dir, CancelToken::new())
    };

    // Resolve provider: try Gemini (if API key set), fall back to mock.
    let provider: std::sync::Arc<dyn mur_gui_core::image_gen::ImageGenProvider> =
        match read_gemini_key() {
            Some(key) => {
                std::sync::Arc::new(GeminiImageGenProvider::new(key, "gemini-2.5-flash-image"))
            }
            None => std::sync::Arc::new(MockImageGenProvider),
        };

    let job = RenderJob::new(preset, provider, &agent_dir);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<RenderProgress>(64);
    let app_clone = app.clone();

    // Forward progress events to the frontend.
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let snap = RenderProgressSnapshot::from(&p);
            let _ = app_clone.emit("wizard-render-progress", &snap);
            // Update session progress.
            if let Ok(mut guard) = app_clone.state::<WizardState>().0.lock()
                && let Some(s) = guard.as_mut()
            {
                s.render_progress = Some(snap);
            }
        }
    });

    // Run render in background; on completion update render_status in agent YAML.
    tokio::spawn(async move {
        let result = job.run(cancel, Some(tx)).await;
        match result {
            Ok(manifest) => {
                let _ = app.emit(
                    "wizard-render-done",
                    serde_json::json!({
                        "expressions_rendered": manifest.expressions.len(),
                        "total": 12,
                    }),
                );
                if let Ok(mut guard) = app.state::<WizardState>().0.lock()
                    && let Some(s) = guard.as_mut()
                {
                    s.render_done = true;
                }
            }
            Err(e) => {
                let _ = app.emit("wizard-render-error", e.to_string());
            }
        }
    });

    Ok(())
}

/// Finish the wizard — create the agent profile YAML and close the session.
#[tauri::command]
pub fn wizard_finish(state: State<'_, WizardState>) -> Result<String, String> {
    let session = state.0.lock().unwrap().take().ok_or("no wizard session")?;

    let name = session.name.ok_or("missing name")?;
    let agent_dir = mur_home_path().join("agents").join(&name);
    std::fs::create_dir_all(&agent_dir).map_err(|e| e.to_string())?;

    let preset_id = session
        .style_preset_id
        .unwrap_or_else(|| "default-blob".into());
    let appearance = AgentAppearance {
        style_preset: preset_id,
        behavior_preset: session.behavior_preset.unwrap_or(BehaviorPreset::Normal),
        source_image_path: session.source_photo,
        expressions_dir: agent_dir.join("expressions"),
        last_rendered_at: None,
        render_status: if session.render_done {
            RenderStatus::Ready
        } else {
            RenderStatus::Pending
        },
    };

    // Persist appearance to agent profile (merge if profile already exists).
    let profile_path = agent_dir.join("profile.yaml");
    if profile_path.exists() {
        let yaml = std::fs::read_to_string(&profile_path).map_err(|e| e.to_string())?;
        let mut profile: mur_common::AgentProfile =
            serde_yaml_ng::from_str(&yaml).map_err(|e| e.to_string())?;
        profile.appearance = appearance;
        let out = serde_yaml_ng::to_string(&profile).map_err(|e| e.to_string())?;
        std::fs::write(&profile_path, out).map_err(|e| e.to_string())?;
    }
    // If no profile exists yet, the agent creation wizard will write it later.

    Ok(name)
}

/// Cancel and discard the current wizard session.
#[tauri::command]
pub fn wizard_cancel(state: State<'_, WizardState>) {
    *state.0.lock().unwrap() = None;
}

/// Render (or re-render) the 12 expressions for an EXISTING agent, outside the
/// onboarding wizard. Used by the detail panel's Style tab so seeded/imported
/// agents stuck at "Not rendered yet" have a working action.
///
/// Persists `render_status` to the agent's profile.yaml (Rendering on start;
/// Ready / Failed on completion) and emits `agent-render-progress`,
/// `agent-render-done`, `agent-render-error` events keyed by agent `name`.
/// Falls back to the offline mock provider when no Gemini key is set, so it
/// always produces a usable result locally.
#[tauri::command]
pub async fn render_agent_expressions(app: AppHandle, name: String) -> Result<(), String> {
    let mur_home = mur_home_path();
    let agent_dir = mur_home.join("agents").join(&name);
    let profile_path = agent_dir.join("profile.yaml");

    let yaml = std::fs::read_to_string(&profile_path).map_err(|e| format!("read profile: {e}"))?;
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&yaml).map_err(|e| format!("parse profile: {e}"))?;
    let hub_dir = mur_home.join("hub");
    let preset_id = profile.appearance.style_preset.clone();
    let preset = find_preset(&preset_id, &hub_dir).unwrap_or_else(|_| default_blob());

    // Mark Rendering and persist so the status survives a reload mid-render.
    profile.appearance.render_status = RenderStatus::Rendering { done: 0, total: 12 };
    write_profile(&profile_path, &profile)?;

    let provider: std::sync::Arc<dyn mur_gui_core::image_gen::ImageGenProvider> =
        match read_gemini_key() {
            Some(key) => {
                std::sync::Arc::new(GeminiImageGenProvider::new(key, "gemini-2.5-flash-image"))
            }
            None => std::sync::Arc::new(MockImageGenProvider),
        };
    let job = RenderJob::new(preset, provider, &agent_dir);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<RenderProgress>(64);
    let app_progress = app.clone();
    let name_progress = name.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let snap = RenderProgressSnapshot::from(&p);
            let _ = app_progress.emit(
                "agent-render-progress",
                serde_json::json!({ "name": name_progress, "progress": snap }),
            );
        }
    });

    let profile_path_done = profile_path.clone();
    tokio::spawn(async move {
        match job.run(CancelToken::new(), Some(tx)).await {
            Ok(manifest) => {
                if let Ok(yaml) = std::fs::read_to_string(&profile_path_done)
                    && let Ok(mut prof) =
                        serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml)
                {
                    prof.appearance.render_status = RenderStatus::Ready;
                    prof.appearance.last_rendered_at = Some(chrono::Utc::now());
                    let _ = write_profile(&profile_path_done, &prof);
                }
                let _ = app.emit(
                    "agent-render-done",
                    serde_json::json!({
                        "name": name,
                        "expressions_rendered": manifest.expressions.len(),
                    }),
                );
            }
            Err(e) => {
                let reason = e.to_string();
                if let Ok(yaml) = std::fs::read_to_string(&profile_path_done)
                    && let Ok(mut prof) =
                        serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml)
                {
                    prof.appearance.render_status = RenderStatus::Failed {
                        reason: reason.clone(),
                    };
                    let _ = write_profile(&profile_path_done, &prof);
                }
                let _ = app.emit(
                    "agent-render-error",
                    serde_json::json!({ "name": name, "reason": reason }),
                );
            }
        }
    });

    Ok(())
}

// ─── Helpers ───────────────────────────────────────────────────────────────

fn write_profile(path: &std::path::Path, profile: &mur_common::AgentProfile) -> Result<(), String> {
    let out = serde_yaml_ng::to_string(profile).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, out).map_err(|e| format!("write {e}"))
}

fn update_session(
    state: &State<'_, WizardState>,
    f: impl FnOnce(&mut WizardSession),
) -> Result<WizardSnapshot, String> {
    let mur_home = mur_home_path();
    let mut guard = state.0.lock().unwrap();
    let session = guard.as_mut().ok_or("no wizard session")?;
    f(session);
    Ok(WizardSnapshot::from_session(session, &mur_home))
}

fn mur_home_path() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".mur")
        })
}

/// Read Gemini API key from the OS env (GEMINI_API_KEY) or the mur secret store.
/// Returns None to fall back to MockImageGenProvider in CI / offline runs.
fn read_gemini_key() -> Option<String> {
    std::env::var("GEMINI_API_KEY").ok()
}
