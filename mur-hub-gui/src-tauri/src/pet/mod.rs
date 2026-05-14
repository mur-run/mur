use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use base64::Engine as _;
use mur_common::hub::trigger::load_triggers;
use mur_gui_core::event_bus::{EventBus, HubEvent};
use mur_gui_core::expression::{ExpressionChange, ExpressionStateMachine};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

// ─── Managed state ──────────────────────────────────────────────────────────

pub struct PetHandle {
    pub window_label: String,
    /// Sending on this channel shuts down the event-loop task.
    pub shutdown_tx: oneshot::Sender<()>,
}

/// Active pet windows: agent_name → handle.
pub struct PetState(pub Mutex<HashMap<String, PetHandle>>);

/// Application-wide event bus (broadcast).
pub struct EventBusState(pub EventBus);

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
    format!(
        "pet-{}",
        agent_name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
    )
}

// ─── Event-loop helpers ──────────────────────────────────────────────────────

fn emit_change(app: &AppHandle, label: &str, change: ExpressionChange) {
    let _ = app.emit_to(
        tauri::EventTarget::labeled(label),
        "pet-expression",
        &change.expression,
    );
    if let Some(text) = change.bubble_text {
        let _ = app.emit_to(tauri::EventTarget::labeled(label), "pet-bubble", &text);
    }
}

/// Spawn a tokio task that drives the ExpressionStateMachine for one pet.
fn start_event_loop(
    app: AppHandle,
    agent_name: String,
    label: String,
    bus: EventBus,
    shutdown_rx: oneshot::Receiver<()>,
) {
    let agent_dir = mur_home().join("agents").join(&agent_name);
    let triggers = load_triggers(&agent_dir);
    let mut sm = ExpressionStateMachine::new(agent_name.clone(), triggers);
    let mut rx = bus.subscribe();

    tokio::spawn(async move {
        tokio::pin!(shutdown_rx);
        let tick_interval = tokio::time::Duration::from_millis(100);
        let mut interval = tokio::time::interval(tick_interval);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                Ok(event) = rx.recv() => {
                    // voice.ended resolves an active lipsync dwell.
                    if event.name == "voice.ended" && event.agent_id == agent_name {
                        if let Some(change) = sm.resolve(mur_common::hub::trigger::DwellSpec::Lipsync) {
                            emit_change(&app, &label, change);
                        }
                    } else if let Some(change) = sm.process(&event) {
                        emit_change(&app, &label, change);
                    }
                }
                _ = interval.tick() => {
                    if let Some(change) = sm.tick(Instant::now()) {
                        emit_change(&app, &label, change);
                    }
                }
            }
        }
    });
}

// ─── Commands ────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn pet_spawn_at(
    agent_name: String,
    screen_x: f64,
    screen_y: f64,
    app: AppHandle,
    state: State<'_, PetState>,
    bus_state: State<'_, EventBusState>,
) -> Result<(), String> {
    let label = window_label(&agent_name);
    let mut pets = state.0.lock().unwrap();

    if pets.contains_key(&agent_name) {
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.set_position(tauri::LogicalPosition::new(screen_x, screen_y));
            let _ = win.show();
            return Ok(());
        }
        pets.remove(&agent_name);
    }

    let pos = load_position(&agent_name).unwrap_or(PetPosition {
        x: screen_x,
        y: screen_y,
        display_id: None,
    });

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

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    pets.insert(
        agent_name.clone(),
        PetHandle {
            window_label: label.clone(),
            shutdown_tx,
        },
    );

    start_event_loop(
        app.clone(),
        agent_name.clone(),
        label.clone(),
        bus_state.0.clone(),
        shutdown_rx,
    );

    // Publish spawn event so the state machine fires the wave sequence.
    let bus = bus_state.0.clone();
    let name = agent_name.clone();
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;
        bus.publish(HubEvent::new(&name, "pet.spawned"));
    });

    Ok(())
}

#[tauri::command]
pub fn pet_close(
    agent_name: String,
    app: AppHandle,
    state: State<'_, PetState>,
) -> Result<(), String> {
    let mut pets = state.0.lock().unwrap();
    if let Some(handle) = pets.remove(&agent_name) {
        let _ = handle.shutdown_tx.send(());
        if let Some(win) = app.get_webview_window(&handle.window_label) {
            win.close().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn pet_reposition(agent_name: String, x: f64, y: f64) {
    save_position(
        &agent_name,
        &PetPosition {
            x,
            y,
            display_id: None,
        },
    );
}

#[tauri::command]
pub fn pet_return_to_hub(
    agent_name: String,
    app: AppHandle,
    state: State<'_, PetState>,
) -> Result<(), String> {
    let mut pets = state.0.lock().unwrap();
    if let Some(handle) = pets.remove(&agent_name) {
        let _ = handle.shutdown_tx.send(());
        if let Some(win) = app.get_webview_window(&handle.window_label) {
            let _ = win.close();
        }
    }
    if let Some(dashboard) = app.get_webview_window("dashboard") {
        let _ = dashboard.show();
        let _ = dashboard.set_focus();
    }
    Ok(())
}

#[tauri::command]
pub fn pet_list(state: State<'_, PetState>) -> Vec<String> {
    state.0.lock().unwrap().keys().cloned().collect()
}

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

/// Publish an arbitrary event onto the bus (called from the pet window UI).
#[tauri::command]
pub fn hub_emit_event(
    agent_name: String,
    event_name: String,
    payload: Option<String>,
    bus_state: State<'_, EventBusState>,
) {
    let mut ev = HubEvent::new(&agent_name, &event_name);
    if let Some(p) = payload {
        ev = ev.with_payload(serde_json::Value::String(p));
    }
    bus_state.0.publish(ev);
}

/// Resolve an `until_ack` dwell (user acknowledged an error bubble).
#[tauri::command]
pub fn pet_ack_bubble(agent_name: String, bus_state: State<'_, EventBusState>) {
    bus_state
        .0
        .publish(HubEvent::new(&agent_name, "pet.bubble.acked"));
}

/// Speak `text` for `agent_name`: synthesise via Kokoro (if models present),
/// play audio (unless Focus/DND active), and alternate talk_open / talk_close
/// on the bus every 200 ms while audio plays.
#[tauri::command]
pub async fn pet_speak(
    agent_name: String,
    text: String,
    voice_id: Option<String>,
    bus_state: State<'_, EventBusState>,
) -> Result<(), String> {
    let bus = bus_state.0.clone();
    let mur_home = mur_home();
    let player = mur_gui_core::voice::VoicePlayer::new(agent_name, mur_home, bus);
    player.speak(text, voice_id).await;
    Ok(())
}

// ─── Helper ──────────────────────────────────────────────────────────────────

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
