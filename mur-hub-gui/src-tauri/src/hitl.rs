use mur_core::a2a_dial::{DialMode, dial_method};
use serde_json::json;

#[tauri::command]
pub fn agent_hitl_respond(name: String, hitl_id: String, allow: bool) -> Result<(), String> {
    let home = crate::mur_home_path();
    dial_method(
        &home,
        &name,
        "tool/hitl_respond",
        json!({ "hitl_id": hitl_id, "allow": allow }),
        DialMode::RequireRunning,
    )
    .map(|_| ())
    .map_err(|e| format!("{e:#}"))
}
