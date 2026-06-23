//! First-run Tauri command: download default local model into writable
//! `~/.mur/models/<name>/` dir, streaming progress to frontend,
//! then start mlx sidecar so concierge can use it.

use tauri::{AppHandle, Emitter};

#[derive(serde::Serialize, Clone)]
struct DownloadProgress {
    done: u64,
    total: u64,
}

#[tauri::command]
pub async fn download_local_model(app: AppHandle) -> Result<(), String> {
    let home = crate::mur_home_path();
    let dest = mur_common::local_llm::local_model_dir(
        &home,
        mur_common::local_llm::DEFAULT_LOCAL_MODEL_DIR,
    );

    let app_clone = app.clone();
    mur_core::model_download::download_hf_model(
        mur_common::local_llm::DEFAULT_LOCAL_MODEL_REPO,
        &dest,
        move |done, total| {
            let _ = app_clone.emit("model-download-progress", DownloadProgress { done, total });
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    // Start local inference first, then tell the UI it can close the picker.
    crate::mlx_sidecar::start(&app);
    app.emit("model-download-done", ()).ok();
    Ok(())
}
