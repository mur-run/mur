use mur_core::a2a_dial::{DialMode, dial_method};
use serde_json::json;

#[tauri::command]
pub fn agent_hitl_respond(
    name: String,
    hitl_id: String,
    allow: bool,
    reason: Option<String>,
) -> Result<(), String> {
    let home = crate::mur_home_path();
    let mut payload = json!({ "hitl_id": hitl_id, "allow": allow });
    if let Some(r) = reason {
        payload["reason"] = json!(r);
    }
    dial_method(
        &home,
        &name,
        "tool/hitl_respond",
        payload,
        DialMode::RequireRunning,
    )
    .map(|_| ())
    .map_err(|e| format!("{e:#}"))
}
