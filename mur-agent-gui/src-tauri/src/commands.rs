//! Tauri command surface — typed RPC between the React frontend and
//! the embedded `mur-core` admin library.
//!
//! Each command is a thin wrapper over `mur_core::agent_admin::*`.
//! Mutators return `Result<(), String>` (Tauri serialises String errors
//! cleanly to the webview); queries return typed values that derive
//! `Serialize`.
//!
//! The agent name is read from `AGENT_NAME` env (set by the bootstrap
//! at first launch when the agent payload is extracted to
//! `~/.mur/agents/<name>/`). Stub here; real wiring lands in P1.3 +
//! P1.6.

use anyhow::Context;
use mur_common::agent::{Entitlements, McpServerEntry};
use mur_common::multimodal::MultimodalArtifact;
use mur_core::agent_admin;
use serde::Serialize;

use crate::multimodal::pipeline::{MultimodalPipeline, PipelineInput};

fn agent_name() -> String {
    std::env::var("MUR_GUI_AGENT_NAME").unwrap_or_else(|_| "template".to_string())
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ─── Status / lifecycle ────────────────────────────────────────────

#[tauri::command]
pub fn status() -> Result<agent_admin::lifecycle::StatusView, String> {
    agent_admin::lifecycle::status(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn start_agent(
    app: tauri::AppHandle,
    mgr: tauri::State<'_, std::sync::Arc<crate::sidecar::SidecarManager>>,
) -> Result<(), String> {
    mgr.start(&app, &agent_name()).map_err(err)
}

#[tauri::command]
pub fn stop_agent(
    mgr: tauri::State<'_, std::sync::Arc<crate::sidecar::SidecarManager>>,
) -> Result<(), String> {
    mgr.stop().map_err(err)?;
    // Also call the agent_admin stop for consistency (it uses
    // running.lock to send SIGTERM in case the sidecar mgr is out
    // of sync with the on-disk state).
    let _ = agent_admin::lifecycle::stop(&agent_name());
    Ok(())
}

#[tauri::command]
pub fn restart_agent(
    app: tauri::AppHandle,
    mgr: tauri::State<'_, std::sync::Arc<crate::sidecar::SidecarManager>>,
) -> Result<(), String> {
    mgr.stop().map_err(err)?;
    mgr.start(&app, &agent_name()).map_err(err)
}

// ─── System Prompt ─────────────────────────────────────────────────

#[tauri::command]
pub fn prompt_get() -> Result<String, String> {
    agent_admin::prompt::get(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn prompt_set(content: String) -> Result<(), String> {
    agent_admin::prompt::set(&agent_name(), Some(&content), None).map_err(err)
}

// ─── Skills ────────────────────────────────────────────────────────

#[tauri::command]
pub fn skill_list() -> Result<Vec<String>, String> {
    agent_admin::skill::list(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn skill_show(query: String) -> Result<String, String> {
    agent_admin::skill::show(&agent_name(), &query).map_err(err)
}

#[tauri::command]
pub fn skill_add(source: String) -> Result<(), String> {
    agent_admin::skill::add(&agent_name(), &source)
        .map(|_id| ())
        .map_err(err)
}

#[tauri::command]
pub fn skill_remove(query: String) -> Result<(), String> {
    agent_admin::skill::remove(&agent_name(), &query).map_err(err)
}

// ─── MCP Servers ───────────────────────────────────────────────────

#[tauri::command]
pub fn mcp_list() -> Result<Vec<McpServerEntry>, String> {
    agent_admin::mcp::list(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn mcp_add(server_id: String, command: String, args: Vec<String>) -> Result<(), String> {
    agent_admin::mcp::add(&agent_name(), &server_id, &command, &args).map_err(err)
}

#[tauri::command]
pub fn mcp_remove(server_id: String) -> Result<(), String> {
    agent_admin::mcp::remove(&agent_name(), &server_id).map_err(err)
}

#[tauri::command]
pub fn mcp_rename(old: String, new: String) -> Result<(), String> {
    agent_admin::mcp::rename(&agent_name(), &old, &new).map_err(err)
}

// ─── Permissions ───────────────────────────────────────────────────

#[tauri::command]
pub fn perm_view() -> Result<Entitlements, String> {
    agent_admin::perm::view(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn perm_set_mode(key: String, value: String) -> Result<(), String> {
    agent_admin::perm::set_mode(&agent_name(), &key, &value).map_err(err)
}

#[tauri::command]
pub fn perm_allow_host(glob: String) -> Result<(), String> {
    agent_admin::perm::allow_host(&agent_name(), &glob).map_err(err)
}

#[tauri::command]
pub fn perm_deny_host(glob: String) -> Result<(), String> {
    agent_admin::perm::deny_host(&agent_name(), &glob).map_err(err)
}

#[tauri::command]
pub fn perm_allow_read(path: String) -> Result<(), String> {
    agent_admin::perm::allow_read(&agent_name(), &path).map_err(err)
}

#[tauri::command]
pub fn perm_allow_write(path: String) -> Result<(), String> {
    agent_admin::perm::allow_write(&agent_name(), &path).map_err(err)
}

#[tauri::command]
pub fn perm_deny_path(path: String) -> Result<(), String> {
    agent_admin::perm::deny_path(&agent_name(), &path).map_err(err)
}

#[tauri::command]
pub fn perm_allow_spawn(binary: String) -> Result<(), String> {
    agent_admin::perm::allow_spawn(&agent_name(), &binary).map_err(err)
}

#[tauri::command]
pub fn perm_deny_spawn(binary: String) -> Result<(), String> {
    agent_admin::perm::deny_spawn(&agent_name(), &binary).map_err(err)
}

#[tauri::command]
pub fn perm_set_limit(key: String, value: u64) -> Result<(), String> {
    agent_admin::perm::set_limit(&agent_name(), &key, value).map_err(err)
}

// ─── Observability ─────────────────────────────────────────────────

#[tauri::command]
pub fn stats() -> Result<agent_admin::observability::StatsView, String> {
    agent_admin::observability::stats(&agent_name()).map_err(err)
}

#[tauri::command]
pub fn logs(tail: usize) -> Result<String, String> {
    agent_admin::observability::logs(&agent_name(), tail).map_err(err)
}

// ─── Theme ─────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ThemeInfo {
    pub name: String,
    pub display_name: String,
    pub kind: String,
}

#[derive(Serialize)]
pub struct AppliedTheme {
    pub name: String,
    pub display_name: String,
    pub kind: String,
    pub colors: std::collections::BTreeMap<String, String>,
}

fn to_applied(def: crate::theme::ThemeDef) -> AppliedTheme {
    let display_name = def
        .display_name
        .get("default")
        .cloned()
        .unwrap_or_else(|| def.name.clone());
    AppliedTheme {
        name: def.name,
        display_name,
        kind: def.kind,
        colors: def.colors,
    }
}

#[tauri::command]
pub fn list_themes(app: tauri::AppHandle) -> Result<Vec<ThemeInfo>, String> {
    use tauri::Manager;
    let resource_dir = app.path().resource_dir().ok();
    let root = crate::theme::resolve_themes_root(resource_dir.as_deref());
    crate::theme::list(&root).map_err(err)
}

#[tauri::command]
pub fn set_theme(app: tauri::AppHandle, name: String) -> Result<AppliedTheme, String> {
    use tauri::Manager;
    let resource_dir = app.path().resource_dir().ok();
    let root = crate::theme::resolve_themes_root(resource_dir.as_deref());
    let def = crate::theme::activate(&root, &name).map_err(err)?;
    Ok(to_applied(def))
}

#[tauri::command]
pub fn get_default_theme(app: tauri::AppHandle) -> Result<AppliedTheme, String> {
    use tauri::Manager;
    let resource_dir = app.path().resource_dir().ok();
    let root = crate::theme::resolve_themes_root(resource_dir.as_deref());
    let name = std::env::var("MUR_GUI_THEME_DEFAULT").unwrap_or_else(|_| "light".to_string());
    let def = crate::theme::activate(&root, &name).map_err(err)?;
    Ok(to_applied(def))
}

// ── Model registry commands (PR-5 / Task 5.1) ─────────────────────────────

#[derive(serde::Serialize)]
pub struct ModelEntryView {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub secret_ref: Option<String>,
    /// `None` = no secret needed; `Some(true/false)` = check() result.
    pub secret_status: Option<bool>,
    pub capabilities: Vec<String>,
}

#[tauri::command]
pub async fn list_models() -> Result<Vec<ModelEntryView>, String> {
    use mur_common::model::ModelRegistry;
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(reg.models.len());
    for (name, e) in &reg.models {
        let secret_status = match &e.secret {
            Some(s) => Some(s.check().await),
            None => None,
        };
        out.push(ModelEntryView {
            name: name.clone(),
            provider: e.provider.clone(),
            model: e.model.clone(),
            base_url: e.base_url.clone(),
            secret_ref: e.secret.as_ref().map(|s| s.to_string()),
            secret_status,
            capabilities: e.capabilities.clone(),
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn get_active_model_ref() -> Result<Option<String>, String> {
    let agent = agent_name();
    let pyaml = dirs::home_dir()
        .ok_or_else(|| "no HOME".to_string())?
        .join(format!(".mur/agents/{agent}/profile.yaml"));
    let body = std::fs::read_to_string(&pyaml).map_err(|e| e.to_string())?;
    let p: mur_common::agent::AgentProfile =
        serde_yaml_ng::from_str(&body).map_err(|e| e.to_string())?;
    Ok(p.model_ref)
}

#[tauri::command]
pub fn set_active_model_ref(name: String) -> Result<(), String> {
    let agent = agent_name();
    let pyaml = dirs::home_dir()
        .ok_or_else(|| "no HOME".to_string())?
        .join(format!(".mur/agents/{agent}/profile.yaml"));
    let body = std::fs::read_to_string(&pyaml).map_err(|e| e.to_string())?;
    let mut p: mur_common::agent::AgentProfile =
        serde_yaml_ng::from_str(&body).map_err(|e| e.to_string())?;
    p.model_ref = Some(name);
    let new = serde_yaml_ng::to_string(&p).map_err(|e| e.to_string())?;
    let tmp = pyaml.with_extension("yaml.tmp");
    std::fs::write(&tmp, new).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &pyaml).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn set_secret(secret: String, value: String) -> Result<(), String> {
    use mur_common::secret::{SecretRef, keychain_set};
    let s: SecretRef = secret
        .parse()
        .map_err(|e: mur_common::secret::SecretError| e.to_string())?;
    match s {
        SecretRef::Keychain { service, account } => keychain_set(&service, &account, &value)
            .await
            .map_err(|e| e.to_string()),
        SecretRef::Env(_) | SecretRef::File(_) | SecretRef::Cmd(_) => {
            Err("set_secret only writes to keychain refs".into())
        }
    }
}

// ─── Voice (D1 / M1) ────────────────────────────────────────────────
//
// Default-off opt-in (per roadmap §4.1 + plan §M1.5.1). Fresh install
// shows no PttButton and no voice picker; Settings → Voice → Enable
// triggers `voice_enable`, which downloads the STT model if missing,
// loads the default voice, registers the PTT hotkey, and persists
// `enabled=true`. `voice_disable` reverses without touching disk
// assets so re-enable is fast.

use crate::voice::VoiceManager;
use std::sync::Arc;
use tokio::sync::RwLock;

pub type VoiceManagerState = Arc<RwLock<VoiceManager>>;

const STT_MODEL_ID: &str = "whisper-large-v3-turbo-q5_1";

#[tauri::command]
pub async fn voice_status(
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<serde_json::Value, String> {
    let mgr = state.read().await;
    let registry = mgr.registry.read().await;
    let stt = mgr.stt.read().await;
    Ok(serde_json::json!({
        "enabled": mgr.is_enabled(),
        "default_voice_id": registry.default_voice_id,
        "voices_installed": registry.list().len(),
        "stt_installed": registry.stt_model_path().is_some(),
        "stt_loaded": stt.is_ready().await,
    }))
}

#[tauri::command]
pub async fn voice_list_installed(
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<serde_json::Value, String> {
    let mgr = state.read().await;
    let registry = mgr.registry.read().await;
    let voices: Vec<_> = registry.list().into_iter().cloned().collect();
    Ok(serde_json::json!({
        "voices": voices,
        "default_voice_id": registry.default_voice_id,
    }))
}

#[tauri::command]
pub async fn voice_set_default(
    voice_id: String,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<(), String> {
    let mgr = state.read().await;
    let mut registry = mgr.registry.write().await;
    registry.set_default(&voice_id).await.map_err(err)
}

#[tauri::command]
pub async fn voice_stt_status(
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<serde_json::Value, String> {
    let mgr = state.read().await;
    let registry = mgr.registry.read().await;
    let installed = registry.stt_model_path().is_some();
    let stt = mgr.stt.read().await;
    Ok(serde_json::json!({
        "model_id": STT_MODEL_ID,
        "installed": installed,
        "loaded": stt.is_ready().await,
        // Display-only; real size comes from the manifest at install time.
        "size_bytes": 809_000_000u64,
    }))
}

/// Download + load the whisper STT model. Idempotent: re-loads if
/// already on disk; downloads if missing. Progress events stream on
/// channel `voice://stt-download-progress`.
#[tauri::command]
pub async fn voice_stt_download(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<(), String> {
    use tauri::Emitter;
    let mgr = state.read().await;
    let registry = mgr.registry.read().await;
    let install_dir = registry.voices_dir().join("_stt").join(STT_MODEL_ID);
    drop(registry);

    let model_path = install_dir.join("ggml-large-v3-turbo-q5_1.bin");
    if !model_path.exists() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let cancel = tokio_util::sync::CancellationToken::new();
        let app2 = app_handle.clone();
        let progress_task = tokio::spawn(async move {
            while let Some(p) = rx.recv().await {
                let _ = app2.emit("voice://stt-download-progress", &p);
            }
        });
        crate::voice::download::download_stt_model(STT_MODEL_ID, install_dir.clone(), tx, cancel)
            .await
            .map_err(err)?;
        // Channel closes when download_stt_model drops `tx`; await
        // the forwarder so any final progress event lands.
        let _ = progress_task.await;
    }

    let stt = mgr.stt.read().await;
    stt.load_model(&model_path).await.map_err(err)
}

/// Per-voice download path. Currently the same flow as `voice_stt_download`
/// but for a voice pack (Kokoro ONNX). Frontend listens on
/// `voice://download-progress`.
#[tauri::command]
pub async fn voice_download(
    voice_id: String,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<(), String> {
    use tauri::Emitter;
    let mgr = state.read().await;
    let registry = mgr.registry.read().await;
    let install_dir = registry.voices_dir().join(&voice_id);
    drop(registry);

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let cancel = tokio_util::sync::CancellationToken::new();
    let app2 = app_handle.clone();
    let progress_task = tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let _ = app2.emit("voice://download-progress", &p);
        }
    });
    crate::voice::download::download_voice(&voice_id, install_dir.clone(), tx, cancel)
        .await
        .map_err(err)?;
    let _ = progress_task.await;

    // Re-verify + register in registry.
    let manifest_bytes = tokio::fs::read(install_dir.join("manifest.json"))
        .await
        .map_err(err)?;
    let sig_bytes = tokio::fs::read(install_dir.join("manifest.json.sig"))
        .await
        .map_err(err)?;
    let bundle =
        crate::voice::manifest::verify_and_parse(&manifest_bytes, &sig_bytes).map_err(err)?;
    let manifest = match bundle {
        crate::voice::manifest::AssetBundle::Voice(v) => v,
        _ => return Err("expected voice manifest, got STT model".into()),
    };
    let mut registry = mgr.registry.write().await;
    registry.install(&manifest, install_dir).await.map_err(err)
}

/// Enable voice. Triggers STT download if missing, loads default voice,
/// registers PTT hotkey, persists `enabled=true`. Idempotent.
#[tauri::command]
pub async fn voice_enable(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<(), String> {
    use tauri::Emitter;
    let mgr = state.read().await;
    if mgr.is_enabled() {
        return Ok(());
    }

    // 1. Download + load STT model (idempotent).
    voice_stt_download(app_handle.clone(), state.clone()).await?;

    // 2. Load default voice if registry has one.
    let registry = mgr.registry.read().await;
    let default_voice = registry
        .default_voice_id
        .as_ref()
        .and_then(|id| registry.get(id))
        .cloned();
    drop(registry);
    if let Some(voice) = default_voice {
        let onnx_path = voice.install_dir.join("voice.onnx");
        if onnx_path.exists() {
            let mut tts = mgr.tts.write().await;
            tts.load_voice(&voice.voice_id, &onnx_path, voice.sample_rate_hz)
                .await
                .map_err(err)?;
        }
    }

    // 3. Register PTT hotkey using the persisted config (or default
    //    if hotkey.json doesn't exist yet — e.g. first enable).
    let cfg = crate::voice::hotkey::load_hotkey(&mgr.app_data_dir).await;
    let shortcut = cfg.to_shortcut().map_err(err)?;
    crate::voice::hotkey::register_shortcut(&app_handle, &shortcut).map_err(err)?;

    // 4. Persist enabled flag.
    mgr.set_enabled(true).await.map_err(err)?;
    let _ = app_handle.emit(
        "voice://state-changed",
        serde_json::json!({ "enabled": true }),
    );
    Ok(())
}

/// Disable voice. Unregisters hotkey, drops in-memory models (frees
/// RAM), persists `enabled=false`. On-disk assets are preserved so
/// re-enable is fast (no re-download).
#[tauri::command]
pub async fn voice_disable(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<(), String> {
    use tauri::Emitter;
    let mgr = state.read().await;
    if !mgr.is_enabled() {
        return Ok(());
    }

    let _ = crate::voice::hotkey::unregister_ptt(&app_handle);
    mgr.tts.write().await.unload();
    mgr.stt.read().await.unload().await;
    mgr.set_enabled(false).await.map_err(err)?;
    let _ = app_handle.emit(
        "voice://state-changed",
        serde_json::json!({ "enabled": false }),
    );
    Ok(())
}

/// Speak a single utterance via the currently-loaded voice. For
/// preview buttons + onboarding "Hi, I'm here" greetings.
#[tauri::command]
pub async fn tts_speak(
    text: String,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<(), String> {
    let mgr = state.read().await;
    let registry = mgr.registry.read().await;
    let voice_id = registry
        .default_voice_id
        .clone()
        .ok_or_else(|| "no default voice".to_string())?;
    let voice = registry
        .get(&voice_id)
        .cloned()
        .ok_or_else(|| "default voice not in registry".to_string())?;
    drop(registry);

    let tts = mgr.tts.read().await;
    let samples = tts
        .synthesize_sentence(&text, &voice.language, 0)
        .await
        .map_err(err)?;
    let sr = tts.sample_rate_hz().await.unwrap_or(voice.sample_rate_hz);
    drop(tts);

    tokio::task::spawn_blocking(move || {
        crate::voice::audio::playback::play_pcm_blocking(&samples, sr)
    })
    .await
    .map_err(|e| format!("playback join: {e}"))?
    .map_err(err)
}

#[tauri::command]
pub async fn stt_transcribe_pcm16k(
    samples_i16: Vec<i16>,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<String, String> {
    let mgr = state.read().await;
    let stt = mgr.stt.read().await;
    stt.transcribe(&samples_i16, None).await.map_err(err)
}

// ─── PTT capture lifecycle (M1.6.2) ─────────────────────────────────
//
// cpal::Stream is !Send on macOS so the stream must live on a
// dedicated OS thread. CaptureWorker (audio/capture_worker.rs) owns
// that thread and exposes Send+Sync stop channels + JoinHandle, which
// is safe to keep in tauri::State.

use crate::voice::audio::capture_worker::CaptureWorker;

pub struct ActiveCapture(pub tokio::sync::Mutex<CaptureWorker>);

impl Default for ActiveCapture {
    fn default() -> Self {
        Self(tokio::sync::Mutex::new(CaptureWorker::new()))
    }
}

/// Get the currently-bound PTT hotkey (persisted or default).
#[tauri::command]
pub async fn voice_get_hotkey(
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<crate::voice::hotkey::HotkeyConfig, String> {
    let mgr = state.read().await;
    Ok(crate::voice::hotkey::load_hotkey(&mgr.app_data_dir).await)
}

/// Re-bind PTT to a new shortcut. Unregisters whatever's currently
/// bound, registers the new combo, persists on success. Frontend
/// captures the raw `KeyboardEvent.code` + modifier flags.
#[tauri::command]
pub async fn voice_rebind_hotkey(
    modifiers: Vec<String>,
    code: String,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, VoiceManagerState>,
) -> Result<crate::voice::hotkey::HotkeyConfig, String> {
    let mgr = state.read().await;
    if !mgr.is_enabled() {
        return Err("voice is disabled — enable first to rebind hotkey".into());
    }
    let cfg = crate::voice::hotkey::HotkeyConfig { modifiers, code };
    crate::voice::hotkey::rebind_ptt(&app_handle, cfg)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn voice_start_capture(
    state: tauri::State<'_, std::sync::Arc<ActiveCapture>>,
) -> Result<(), String> {
    let mut w = state.0.lock().await;
    w.start().map_err(err)
}

#[tauri::command]
pub async fn voice_stop_capture(
    state: tauri::State<'_, std::sync::Arc<ActiveCapture>>,
) -> Result<Vec<i16>, String> {
    let mut w = state.0.lock().await;
    w.stop().map_err(err)
}

// ─── D2 onboarding (M2.4) ──────────────────────────────────────────
//
// Three commands back the GUI's onboarding wizard:
//   * `companion_onboarding_status` — reads the persisted state so the
//     wizard can route to "show wizard" vs "show summary" on launch
//   * `companion_onboarding_submit` — invokes the same `init::run` path
//     the CLI uses (`mur agent companion init --answers`)
//   * `companion_onboarding_skip`   — closes the door for users who
//     don't want a relationship-keyed agent: marks `completed_at = now`
//     and applies the WarmOnly tier (Spec §4.2 default)
//
// All three resolve `~/.mur` via `mur_core::paths::mur_root` so the
// `MUR_HOME` override (used in tests + first-launch bootstrap) works.
// Helpers split out of the `#[tauri::command]` wrappers so integration
// tests can call them without a webview.

fn onboarding_agent_dir(agent: &str) -> anyhow::Result<std::path::PathBuf> {
    let dir = mur_core::paths::mur_root(None).join("agents").join(agent);
    if !dir.exists() {
        anyhow::bail!(
            "agent {agent} does not exist at {} — run `mur agent create {agent}` first",
            dir.display()
        );
    }
    Ok(dir)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OnboardingStatus {
    pub completed_at: Option<String>,
    pub agent_display_name: Option<String>,
    pub first_memory_text: Option<String>,
    /// One of `off` | `warm_only` | `warm_and_behavior` | `all`,
    /// derived from `companion.{enabled, rhythm.enabled, proactive.enabled}`
    /// via `ProactiveTier::from_config`. Lower-case so the React wizard
    /// can match on the same string the submit payload uses.
    pub proactive_tier: String,
}

pub async fn companion_onboarding_status_impl(agent: &str) -> anyhow::Result<OnboardingStatus> {
    let dir = onboarding_agent_dir(agent)?;
    let body = tokio::fs::read_to_string(dir.join("profile.yaml")).await?;
    let p: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&body)?;
    let tier = mur_common::agent::ProactiveTier::from_config(&p.companion);
    let tier_str = serde_json::to_value(tier)?
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "off".to_string());
    Ok(OnboardingStatus {
        completed_at: p.companion.onboarding.completed_at.map(|t| t.to_rfc3339()),
        agent_display_name: p.companion.onboarding.agent_display_name.clone(),
        first_memory_text: p
            .companion
            .onboarding
            .first_memory
            .as_ref()
            .map(|f| f.text.clone()),
        proactive_tier: tier_str,
    })
}

#[tauri::command]
pub async fn companion_onboarding_status(agent: String) -> Result<OnboardingStatus, String> {
    companion_onboarding_status_impl(&agent).await.map_err(err)
}

/// Wizard payload — mirrors the CLI's `--answers` YAML shape one-for-one.
/// Fields are validated downstream when `init::run` deserialises the
/// generated YAML, so this struct stays a flat string-typed bag.
///
/// `re_init` defaults to `false` (matching the CLI safety default).
/// The frontend must explicitly set it to `true` when re-running the
/// wizard from the "Edit" entrypoint so a user's first_memory cannot be
/// silently overwritten by an accidental wizard re-entry.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct OnboardingSubmit {
    pub agent_display_name: String,
    pub locale: String,
    pub name_for_user: String,
    /// One of `friend` | `coach` | `accountability_buddy` | `mentor`.
    pub relationship: String,
    pub first_memory: Option<String>,
    /// One of `off` | `warm_only` | `warm_and_behavior` | `all`.
    pub proactive_tier: String,
    /// `true` only when the wizard is launched from the "Edit" button
    /// against an already-onboarded agent. Default `false`.
    #[serde(default)]
    pub re_init: bool,
}

pub async fn companion_onboarding_submit_impl(
    agent: &str,
    p: OnboardingSubmit,
) -> anyhow::Result<()> {
    use std::io::Write;
    // Build the `--answers` payload as JSON-via-serde_json::Value first so
    // we can omit `first_memory` cleanly when absent (a literal `null` in
    // the YAML deserialises as `Some("null"-ish)` — but `serde_yaml_ng`
    // round-trips `Value::Null` to a real null, which `init::run` then
    // sees as `None` per `#[serde(default)]`).
    let mut obj = serde_json::Map::new();
    obj.insert("locale".into(), serde_json::Value::String(p.locale));
    obj.insert(
        "name_for_user".into(),
        serde_json::Value::String(p.name_for_user),
    );
    obj.insert(
        "agent_display_name".into(),
        serde_json::Value::String(p.agent_display_name),
    );
    obj.insert(
        "relationship".into(),
        serde_json::Value::String(p.relationship),
    );
    obj.insert(
        "formality".into(),
        serde_json::Value::String("casual".into()),
    );
    obj.insert(
        "extra_instructions".into(),
        serde_json::Value::String(String::new()),
    );
    if let Some(text) = p.first_memory {
        obj.insert("first_memory".into(), serde_json::Value::String(text));
    }
    obj.insert(
        "proactive_tier".into(),
        serde_json::Value::String(p.proactive_tier),
    );
    let yaml = serde_yaml_ng::to_string(&serde_json::Value::Object(obj))?;

    // Use a NamedTempFile so the file is unlinked on drop even if
    // `init::run` panics. The CLI parses by path, so we hand it the
    // path (not a handle) and keep the temp file alive for the call.
    let tmp = tempfile::NamedTempFile::new()?;
    tmp.as_file().write_all(yaml.as_bytes())?;
    tmp.as_file().sync_all().ok();
    let answers_path = tmp.path().to_path_buf();
    // re-init defaults off — the frontend must set re_init=true via the
    // "Edit" entrypoint to overwrite an existing onboarding state, so
    // the wizard's normal first-launch path can never silently clobber
    // a user's first_memory.
    mur_core::cmd::agent_companion::init::run(agent, Some(answers_path), p.re_init).await?;
    drop(tmp);
    Ok(())
}

#[tauri::command]
pub async fn companion_onboarding_submit(
    agent: String,
    payload: OnboardingSubmit,
) -> Result<(), String> {
    companion_onboarding_submit_impl(&agent, payload)
        .await
        .map_err(err)
}

pub async fn companion_onboarding_skip_impl(agent: &str) -> anyhow::Result<()> {
    let dir = onboarding_agent_dir(agent)?;
    let pf_path = dir.join("profile.yaml");
    let body = tokio::fs::read_to_string(&pf_path).await?;
    let mut p: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&body)?;
    // WarmOnly tier — flips `companion.enabled` true so voice composition
    // works, leaves rhythm + proactive dormant. Same default the 5-step
    // wizard lands on for users who hit Enter through every step.
    mur_common::agent::ProactiveTier::WarmOnly.apply(&mut p.companion);
    p.companion.onboarding.completed_at = Some(chrono::Utc::now());
    p.companion.onboarding.version = 1;
    let yaml = serde_yaml_ng::to_string(&p)?;
    // Atomic write: temp file + rename, mirroring `cmd::agent_companion::util`.
    let tmp_path = pf_path.with_extension("yaml.tmp");
    tokio::fs::write(&tmp_path, yaml).await?;
    tokio::fs::rename(&tmp_path, &pf_path).await?;
    Ok(())
}

#[tauri::command]
pub async fn companion_onboarding_skip(agent: String) -> Result<(), String> {
    companion_onboarding_skip_impl(&agent).await.map_err(err)
}

// ─── D3 multimodal drag-drop / paste (M3.6) ────────────────────────
//
// Two helpers (`_impl`) back the `#[tauri::command]` wrappers so they
// can be unit-tested without a webview. Caps are enforced *before*
// any pipeline work runs:
//
//   * `MAX_FILES_PER_DROP` — fail-fast on glob-drop accidents
//   * `MAX_TOTAL_BYTES_PER_DROP` — keep one drop bounded
//
// After caps the pipeline does HEIC normalize, sandboxed decode, OCR,
// Unicode scrub, provenance ledger entry. Step 1 (dedupe) lives in
// `main.rs`'s `on_window_event`; step 7 (`untrusted_image_text`) and
// step 9 (turn-flag) land in M3.8 with the `B0SafetyHook`.

const MAX_FILES_PER_DROP: usize = 10;
const MAX_TOTAL_BYTES_PER_DROP: u64 = 30 * 1024 * 1024;

pub async fn multimodal_drop_impl(
    agent: &str,
    paths: Vec<std::path::PathBuf>,
    turn_id: u64,
) -> anyhow::Result<Vec<MultimodalArtifact>> {
    if paths.len() > MAX_FILES_PER_DROP {
        anyhow::bail!(
            "multimodal_drop: max 10 files per drop, got {}",
            paths.len()
        );
    }
    let mut total_bytes = 0u64;
    for p in &paths {
        let m = std::fs::metadata(p).with_context(|| format!("metadata {}", p.display()))?;
        total_bytes += m.len();
    }
    if total_bytes > MAX_TOTAL_BYTES_PER_DROP {
        anyhow::bail!("multimodal_drop: total > 30 MB ({total_bytes} bytes)");
    }
    let agent_home = onboarding_agent_dir(agent)?;
    let pipeline = MultimodalPipeline::new(agent_home, turn_id);
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let a = pipeline
            .process(PipelineInput::Path {
                path: p,
                source: "user_drop".into(),
            })
            .await?;
        out.push(a);
    }
    Ok(out)
}

#[tauri::command]
pub async fn multimodal_drop(
    agent: String,
    paths: Vec<std::path::PathBuf>,
    turn_id: u64,
) -> Result<Vec<MultimodalArtifact>, String> {
    multimodal_drop_impl(&agent, paths, turn_id)
        .await
        .map_err(err)
}

// ─── M3.6.2 — `multimodal_paste` (clipboard → pipeline) ────────────
//
// Two-tier clipboard reader:
//   1. `MUR_TEST_CLIPBOARD_IMAGE` env var (test bypass — points at a
//      file whose bytes we treat as the clipboard image)
//   2. real Tauri clipboard plugin (TODO — `tauri-plugin-clipboard-manager`
//      gates the image accessor behind platform-specific code paths
//      which we wire in M3.7+/M3.9 alongside the React surface)
//
// If neither yields bytes we surface a `clipboard has no image` error
// so the React side can show a polite message.

pub async fn multimodal_paste_impl(
    agent: &str,
    turn_id: u64,
) -> anyhow::Result<Vec<MultimodalArtifact>> {
    let bytes = read_clipboard_image_for_test()
        .or_else(read_clipboard_image_via_plugin)
        .ok_or_else(|| anyhow::anyhow!("clipboard has no image"))?;
    if bytes.len() as u64 > MAX_TOTAL_BYTES_PER_DROP {
        anyhow::bail!("clipboard image > 30 MB");
    }
    let agent_home = onboarding_agent_dir(agent)?;
    let pipeline = MultimodalPipeline::new(agent_home, turn_id);
    let a = pipeline
        .process(PipelineInput::Bytes {
            bytes,
            mime_hint: "image/png".into(),
            source: "user_paste".into(),
        })
        .await?;
    Ok(vec![a])
}

#[tauri::command]
pub async fn multimodal_paste(
    agent: String,
    turn_id: u64,
) -> Result<Vec<MultimodalArtifact>, String> {
    multimodal_paste_impl(&agent, turn_id).await.map_err(err)
}

fn read_clipboard_image_for_test() -> Option<Vec<u8>> {
    std::env::var("MUR_TEST_CLIPBOARD_IMAGE")
        .ok()
        .and_then(|p| std::fs::read(p).ok())
}

fn read_clipboard_image_via_plugin() -> Option<Vec<u8>> {
    // Real Tauri clipboard wiring is platform-specific (the manager
    // plugin's image accessor differs across macOS/Linux/Windows).
    // For M3.6.2 we ship the test bypass; the production path is a
    // TODO tracked alongside the M3.6 milestone (lands with M3.7
    // when the React side wires up the paste handler).
    None
}
