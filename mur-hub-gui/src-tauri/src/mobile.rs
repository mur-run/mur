use std::path::PathBuf;

/// Read the raw contents of `~/.mur/agents/<agent>/mobile-events.jsonl`.
/// Returns an empty string if the file doesn't exist yet.
#[tauri::command]
pub async fn mobile_events_read(agent_name: String) -> Result<String, String> {
    let path = mobile_events_path(&agent_name);
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

fn mobile_events_path(agent: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".mur")
        .join("agents")
        .join(agent)
        .join("mobile-events.jsonl")
}
