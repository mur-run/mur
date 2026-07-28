pub mod first_launch;
pub mod spec;
pub use first_launch::{check_first_launch, mark_first_launch_done, replay_onboarding};

use mur_common::agent::RenderStatus;
use mur_common::hub::preset_loader::{default_blob, find_preset};
use mur_gui_core::image_gen::gemini::GeminiImageGenProvider;
use mur_gui_core::image_gen::{CancelToken, RenderProgress};
use mur_gui_core::render::RenderJob;
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

/// Render progress as emitted to the UI on `agent-render-progress`.
#[derive(Debug, Clone, Serialize)]
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

// ─── Tauri commands ────────────────────────────────────────────────────────

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

    // No image model → built-in vector mascot. Write a vector manifest, mark
    // Ready, and return — no flat-colour mock art is generated.
    let Some(key) = read_gemini_key() else {
        write_vector_manifest(&agent_dir, &preset)?;
        profile.appearance.render_status = RenderStatus::Ready;
        profile.appearance.last_rendered_at = Some(chrono::Utc::now());
        write_profile(&profile_path, &profile)?;
        let _ = app.emit(
            "agent-render-done",
            serde_json::json!({ "name": name, "expressions_rendered": 0, "mode": "vector" }),
        );
        return Ok(());
    };

    // Mark Rendering and persist so the status survives a reload mid-render.
    profile.appearance.render_status = RenderStatus::Rendering { done: 0, total: 12 };
    write_profile(&profile_path, &profile)?;

    let provider: std::sync::Arc<dyn mur_gui_core::image_gen::ImageGenProvider> =
        std::sync::Arc::new(GeminiImageGenProvider::new(key, "gemini-2.5-flash-image"));
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
                    && let Ok(mut prof) = serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml)
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
                    && let Ok(mut prof) = serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml)
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

/// Write a `mode: Vector` manifest (no `.webp` files) for `agent_dir`. Used when
/// no image model is available, so the pet renders the built-in vector mascot
/// (`PetFace`) — distinct per style and instant — instead of a flat-colour mock.
fn write_vector_manifest(
    agent_dir: &std::path::Path,
    preset: &mur_common::hub::style_preset::StylePreset,
) -> Result<(), String> {
    use mur_common::hub::preset_manifest::{
        PresetManifest, RenderMode, compute_preset_hash, manifest_path,
    };
    let dir = agent_dir.join("expressions");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create expressions dir: {e}"))?;
    let manifest = PresetManifest {
        preset_id: preset.id.clone(),
        rendered_at: chrono::Utc::now(),
        sha256: compute_preset_hash(preset),
        expressions: preset.expressions.iter().map(|e| e.id.clone()).collect(),
        mode: RenderMode::Vector,
    };
    let json =
        serde_json::to_string_pretty(&manifest).map_err(|e| format!("serialize manifest: {e}"))?;
    std::fs::write(manifest_path(agent_dir), json).map_err(|e| format!("write manifest: {e}"))
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
