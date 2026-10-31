//! Voice (D1) and push-to-talk capture commands. Moved verbatim from `commands.rs`.

use super::err;

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
