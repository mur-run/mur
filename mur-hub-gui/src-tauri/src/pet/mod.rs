use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

// ─── Managed state ──────────────────────────────────────────────────────────

/// Active pet windows: agent_name → window_label.
pub struct PetState(pub Mutex<HashMap<String, String>>);

// ─── Position persistence ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetPosition {
    pub x: f64,
    pub y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,
}

fn mur_home() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".mur"))
}

fn pet_position_path(agent_name: &str) -> PathBuf {
    mur_home()
        .join("agents")
        .join(agent_name)
        .join("pet_position.json")
}

fn load_position(agent_name: &str) -> Option<PetPosition> {
    let data = std::fs::read_to_string(pet_position_path(agent_name)).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_position(agent_name: &str, pos: &PetPosition) {
    let path = pet_position_path(agent_name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(pos) {
        let _ = std::fs::write(path, json);
    }
}

fn window_label(agent_name: &str) -> String {
    // Window labels must be ASCII alphanumeric + hyphen/underscore.
    format!(
        "pet-{}",
        agent_name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>()
    )
}

// ─── Commands ────────────────────────────────────────────────────────────────

/// Spawn a transparent always-on-top pet window for `agent_name` at the given
/// screen-logical coordinates.  If the pet is already open, move it instead.
#[tauri::command]
pub fn pet_spawn_at(
    agent_name: String,
    screen_x: f64,
    screen_y: f64,
    app: AppHandle,
    state: State<'_, PetState>,
) -> Result<(), String> {
    let label = window_label(&agent_name);
    let mut pets = state.0.lock().unwrap();

    // Already open — move and ensure visible.
    if pets.contains_key(&agent_name) {
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.set_position(tauri::LogicalPosition::new(screen_x, screen_y));
            let _ = win.show();
            return Ok(());
        }
        pets.remove(&agent_name);
    }

    // Use saved position if available, otherwise the drop position.
    let pos = load_position(&agent_name)
        .unwrap_or(PetPosition { x: screen_x, y: screen_y, display_id: None });

    // Build the window URL: index.html#/pet/<name>
    let url_path = format!("index.html#/pet/{}", urlenc(&agent_name));

    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url_path.into()))
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible_on_all_workspaces(true)
        .shadow(false)
        .inner_size(200.0, 200.0)
        .position(pos.x, pos.y)
        .build()
        .map_err(|e| e.to_string())?;

    pets.insert(agent_name.clone(), label.clone());

    // wave → idle sequence after window has had time to render.
    let app2 = app.clone();
    let label2 = label.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let _ = app2.emit_to(tauri::EventTarget::labeled(&label2), "pet-expression", "wave");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let _ = app2.emit_to(tauri::EventTarget::labeled(&label2), "pet-expression", "idle");
    });

    Ok(())
}

/// Close the pet window for `agent_name`.
#[tauri::command]
pub fn pet_close(
    agent_name: String,
    app: AppHandle,
    state: State<'_, PetState>,
) -> Result<(), String> {
    let mut pets = state.0.lock().unwrap();
    if let Some(label) = pets.remove(&agent_name) {
        if let Some(win) = app.get_webview_window(&label) {
            win.close().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Persist the new position after a drag-reposition.
#[tauri::command]
pub fn pet_reposition(
    agent_name: String,
    x: f64,
    y: f64,
) {
    save_position(&agent_name, &PetPosition { x, y, display_id: None });
}

/// Close the pet window and surface the Hub dashboard.
#[tauri::command]
pub fn pet_return_to_hub(
    agent_name: String,
    app: AppHandle,
    state: State<'_, PetState>,
) -> Result<(), String> {
    let mut pets = state.0.lock().unwrap();
    if let Some(label) = pets.remove(&agent_name) {
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.close();
        }
    }
    if let Some(dashboard) = app.get_webview_window("dashboard") {
        let _ = dashboard.show();
        let _ = dashboard.set_focus();
    }
    Ok(())
}

/// Names of currently-open pet windows.
#[tauri::command]
pub fn pet_list(state: State<'_, PetState>) -> Vec<String> {
    state.0.lock().unwrap().keys().cloned().collect()
}

/// Load an expression image and return it as a `data:image/webp;base64,…` URL.
/// Returns an empty string if the file does not exist (UI falls back to initials).
#[tauri::command]
pub fn pet_get_expression(agent_name: String, expression: String) -> String {
    let path = mur_home()
        .join("agents")
        .join(&agent_name)
        .join("expressions")
        .join(format!("{expression}.webp"));

    std::fs::read(&path)
        .map(|b| {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&b);
            format!("data:image/webp;base64,{b64}")
        })
        .unwrap_or_default()
}

// ─── Helper ──────────────────────────────────────────────────────────────────

/// Percent-encode a string for use in a URL path segment (spaces → %20 only).
fn urlenc(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c == ' ' {
                vec!['%', '2', '0']
            } else {
                vec![c]
            }
        })
        .collect()
}
