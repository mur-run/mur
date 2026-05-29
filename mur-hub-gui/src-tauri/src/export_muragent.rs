//! `mur agent export` — Hub side. Produces a `.muragent` from an installed
//! agent using the exact CLI path (template/sanitized). Spec §6 (C).

use std::path::Path;

#[tauri::command]
pub fn export_muragent_file(name: String, out_path: String) -> Result<String, String> {
    mur_core::cmd::agent::export::export_agent_to_muragent(&name, Path::new(&out_path))
        .map_err(|e| e.to_string())?;
    Ok(out_path)
}
