//! Fleet Pause / Resume for MUR Hub (pause/resume design §4): the same core
//! entry points `mur fleet pause` / `mur fleet resume --detach` use, so the
//! Hub adds no admission rules of its own.

use std::path::PathBuf;

use mur_core::cmd::fleet::loop_run::resume_detach;
use mur_core::cmd::fleet::pause::{self, PausePhase};

use crate::mur_home_path;

/// The pause phase as the UI reads it: `"none" | "pausing" | "paused"`.
pub(crate) fn phase_str(name: &str) -> &'static str {
    match pause::phase(&mur_home_path(), name) {
        PausePhase::None => "none",
        PausePhase::Pausing => "pausing",
        PausePhase::Paused => "paused",
    }
}

/// The `mur` a detached resume re-executes. The Hub's own executable is the
/// Hub, so `current_exe()` is wrong here. The bundled sibling comes first: it
/// is the same build as the core this Hub links, so it knows `fleet resume`;
/// a `mur` on PATH may be older.
fn mur_binary() -> Result<PathBuf, String> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| crate::cli_tools::bundled_mur_path(&exe))
        .filter(|p| p.is_file())
        .or_else(crate::cli_tools::resolve_mur)
        .ok_or_else(|| "cannot find the `mur` binary to resume with".to_string())
}

/// Pause the fleet's live run. Waits for the safe point like the CLI does
/// (up to its own bound), so it runs off the async workers.
#[tauri::command]
pub async fn fleet_pause(name: String) -> Result<(), String> {
    let home = mur_home_path();
    tokio::task::spawn_blocking(move || pause::cmd_fleet_pause(&home, &name))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Resume the paused run in a detached child; returns the operator message.
/// Never a daemon path: stdin is null, so an unanswered HITL gate defers.
#[tauri::command]
pub async fn fleet_resume(name: String) -> Result<String, String> {
    let home = mur_home_path();
    let exe = mur_binary()?;
    tokio::task::spawn_blocking(move || resume_detach::resume_detached(&home, &name, &exe))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}
