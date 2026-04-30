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

use mur_common::agent::{Entitlements, McpServerEntry};
use mur_core::agent_admin;
use serde::Serialize;

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
    agent_admin::skill::add(&agent_name(), &source).map_err(err)
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

    // 3. Register PTT hotkey.
    crate::voice::hotkey::register_ptt(&app_handle).map_err(err)?;

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
