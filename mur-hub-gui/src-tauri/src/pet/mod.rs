use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use base64::Engine as _;
use mur_common::hub::trigger::load_triggers;
use mur_gui_core::event_bus::{EventBus, HubEvent};
use mur_gui_core::expression::{ExpressionChange, ExpressionStateMachine};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

use crate::geometry;

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

fn mur_home() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".mur"))
}

/// Physical-pixel rect of the monitor containing `(x, y)`, falling back to the
/// primary monitor, then a 1440×900 origin rect if no monitor is reported.
pub(crate) fn monitor_rect_for_point(app: &AppHandle, x: i32, y: i32) -> geometry::Rect {
    let to_rect = |m: &tauri::Monitor| {
        let p = m.position();
        let s = m.size();
        geometry::Rect {
            x: p.x,
            y: p.y,
            w: s.width as i32,
            h: s.height as i32,
        }
    };
    if let Ok(mons) = app.available_monitors() {
        if let Some(m) = mons.iter().find(|m| {
            let r = to_rect(m);
            x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
        }) {
            return to_rect(m);
        }
        // Fall back to the primary monitor.
        if let Ok(Some(m)) = app.primary_monitor() {
            return to_rect(&m);
        }
    }
    // Last-resort fallback: a sensible 1440×900 origin rect.
    geometry::Rect {
        x: 0,
        y: 0,
        w: 1440,
        h: 900,
    }
}

/// True if `agent_name` is safe to use as a path component under
/// `~/.mur/agents/` (canonical agent-name rules).
fn valid_agent(agent_name: &str) -> bool {
    mur_common::agent_name::validate_agent_name(agent_name).is_ok()
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

    // Use Tauri's managed runtime: pet_spawn_at is a SYNC #[tauri::command] that
    // runs on a thread with no entered Tokio runtime, so a bare tokio::spawn here
    // panics ("there is no reactor running"). async_runtime::spawn holds the runtime
    // handle; tokio::time/select inside then run on that runtime.
    tauri::async_runtime::spawn(async move {
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

    // The explicit drop point always wins. DOM drop coords (screen_x/y) and
    // PET_W/PET_H are LOGICAL; the geometry
    // module + monitor rects are PHYSICAL. Convert with the monitor scale.
    // ponytail: uniform-scale assumption (primary monitor's factor); mixed-DPI
    // multi-monitor would need per-monitor scale, rare — revisit if it bites.
    let scale = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(1.0);
    let phys_x = (screen_x * scale) as i32;
    let phys_y = (screen_y * scale) as i32;
    let mon = monitor_rect_for_point(&app, phys_x, phys_y);
    let pet_w = (PET_W as f64 * scale) as i32;
    let pet_h = (PET_H as f64 * scale) as i32;
    let (cx, cy) = geometry::clamp_into((phys_x, phys_y), (pet_w, pet_h), mon);

    let url_path = format!("index.html#/pet/{}", urlenc(&agent_name));

    let win = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url_path.into()))
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible_on_all_workspaces(true)
        .shadow(false)
        .inner_size(PET_W as f64, PET_H as f64)
        .visible(false) // Task 6: avoid opaque-square entrance flash
        .build()
        .map_err(|e| e.to_string())?;
    let _ = win.set_position(tauri::PhysicalPosition::new(cx, cy));
    let _ = win.show();

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
    tauri::async_runtime::spawn(async move {
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

/// Open `agent_name`'s chat panel. The (hidden) dashboard webview relays the
/// `pet-open-chat` event to `open_chat_window`. NOTE: the `draft` is emitted
/// but not yet consumed by the chat composer (file-drop draft prefill is
/// deferred to Phase 3); do NOT show/focus the dashboard here — that caused
/// the Hub to "jump" alongside the chat window.
#[tauri::command]
pub fn pet_open_chat(agent_name: String, draft: Option<String>, app: AppHandle) {
    let _ = app.emit(
        "pet-open-chat",
        serde_json::json!({ "agent": agent_name, "draft": draft }),
    );
}

// ─── File drop ─────────────────────────────────────────────────────────────

/// Physical-pixel pet window size; the single source for both inner_size and clamp_into.
const PET_W: i32 = 300;
const PET_H: i32 = 260;

const PET_DROP_MAX_FILES: usize = 5;
const PET_DROP_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024;
const PET_DROP_MAX_CHARS_PER_FILE: usize = 8000;
/// Hard per-file read ceiling (well above the char budget) so a pseudo/special
/// file can never stream unbounded into memory.
const PET_DROP_READ_BYTE_CAP: u64 = 256 * 1024;

/// Read at most `max_bytes` from `path` as lossy UTF-8. Returns None if the file
/// can't be opened/read. Bounded so a symlink to e.g. /dev/zero can't OOM us.
fn read_text_capped(path: &std::path::Path, max_bytes: u64) -> Option<String> {
    use std::io::Read as _;
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(max_bytes).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Extensions we can safely read as UTF-8 text for an inline quick-take.
fn is_text_like(path: &std::path::Path) -> bool {
    const TEXT_EXTS: &[&str] = &[
        "txt", "md", "markdown", "rst", "log", "csv", "tsv", "json", "yaml", "yml", "toml", "ini",
        "cfg", "conf", "xml", "html", "htm", "css", "scss", "sh", "bash", "zsh", "rs", "py", "js",
        "ts", "tsx", "jsx", "go", "c", "h", "cpp", "hpp", "cc", "java", "rb", "php", "sql", "kt",
        "swift", "lua", "r", "pl", "tex", "env",
    ];
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => TEXT_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Concatenate the text parts of the last agent message in an A2A task result.
fn extract_reply(task: &serde_json::Value) -> String {
    task.get("messages")
        .and_then(|m| m.as_array())
        .and_then(|msgs| {
            msgs.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("agent"))
        })
        .map(|m| {
            m.get("parts")
                .and_then(|p| p.as_array())
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Result of dropping file(s) on a pet: the inline quick-take reply (shown as a
/// bubble) plus what was read vs skipped.
#[derive(Debug, Clone, Serialize)]
pub struct PetDropResult {
    pub reply: String,
    pub text_files: usize,
    pub skipped: Vec<String>,
    /// Display name of the non-local provider receiving the file contents, or
    /// `None` when the agent is using a demonstrably local model (ollama, mlx,
    /// lmstudio, or a loopback base_url). Used for the privacy disclosure bubble.
    pub remote_provider: Option<String>,
}

/// Return a friendly provider display name when the provider is known to be
/// a cloud service, or `None` when it is demonstrably local.
///
/// "Demonstrably local" = provider key is a known local runtime, OR the
/// resolved base_url is a loopback address. If we cannot determine locality
/// (e.g. an unknown custom provider), we disclose generically — fail toward
/// disclosure.
fn resolve_remote_provider(agent_name: &str) -> Option<String> {
    let home = mur_home();
    let profile_path = home.join("agents").join(agent_name).join("profile.yaml");
    let yaml = match std::fs::read_to_string(&profile_path) {
        Ok(y) => y,
        Err(_) => return Some("the agent's model".into()), // fail-toward-disclosure
    };
    let profile: mur_common::AgentProfile = match serde_yaml_ng::from_str(&yaml) {
        Ok(p) => p,
        Err(_) => return Some("the agent's model".into()),
    };

    // If the profile has a model_ref, look it up in models.yaml for a richer
    // base_url check; fall back to inline model.provider.
    let (provider, base_url): (String, Option<String>) = if let Some(ref mref) = profile.model_ref {
        let registry_path = home.join("models.yaml");
        if let Ok(registry) = mur_common::model::ModelRegistry::load_from(&registry_path) {
            if let Some(entry) = registry.models.get(mref) {
                (entry.provider.clone(), entry.base_url.clone())
            } else {
                (profile.model.provider.clone(), None)
            }
        } else {
            (profile.model.provider.clone(), None)
        }
    } else {
        (profile.model.provider.clone(), None)
    };

    // A base_url pointing at loopback is always local regardless of provider name.
    if let Some(ref url) = base_url {
        if url.contains("127.0.0.1") || url.contains("localhost") || url.contains("[::1]") || url.contains("0.0.0.0") {
            return None;
        }
    }

    // Known local runtimes.
    match provider.as_str() {
        "ollama" | "mlx" | "lmstudio" => None,
        // Known cloud providers — return a friendly display name.
        "anthropic" => Some("Anthropic".into()),
        "openai" => Some("OpenAI".into()),
        "openrouter" => Some("OpenRouter".into()),
        "gemini" => Some("Google Gemini".into()),
        // Unknown provider with no loopback URL: disclose generically.
        _ => Some("the agent's model".into()),
    }
}

/// Handle file(s) dropped onto `agent_name`'s pet. Reads text-like files, stages
/// their full content as a draft in the Hub conversation (for follow-up), and
/// asks the agent for a brief inline take returned to the pet as a bubble.
/// Non-text files (images/binaries) can't be read by the single-turn send path,
/// so they only open the chat with a reference note.
#[tauri::command]
pub async fn pet_drop_files(
    agent_name: String,
    paths: Vec<String>,
    app: AppHandle,
) -> Result<PetDropResult, String> {
    if !valid_agent(&agent_name) {
        return Err("invalid agent name".into());
    }

    let mut sections = Vec::new();
    let mut skipped = Vec::new();
    let mut total = 0u64;
    // Honest truncation: if the drop exceeds the file cap, push a synthetic entry
    // so the user sees "+N more (max M)" rather than a silent drop.
    if paths.len() > PET_DROP_MAX_FILES {
        skipped.push(format!(
            "+{} more (max {})",
            paths.len() - PET_DROP_MAX_FILES,
            PET_DROP_MAX_FILES
        ));
    }
    for p in paths.iter().take(PET_DROP_MAX_FILES) {
        let path = std::path::Path::new(p);
        let fname = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.clone());
        if !is_text_like(path) {
            skipped.push(fname);
            continue;
        }
        // Bounded read: never trust metadata().len() (pseudo-files like a
        // symlink to /dev/zero report 0 yet read forever → OOM). Cap the bytes
        // actually read, then truncate to the char budget.
        let Some(mut c) = read_text_capped(path, PET_DROP_READ_BYTE_CAP) else {
            skipped.push(fname);
            continue;
        };
        total = total.saturating_add(c.len() as u64);
        if total > PET_DROP_MAX_TOTAL_BYTES {
            skipped.push(fname);
            break;
        }
        if c.chars().count() > PET_DROP_MAX_CHARS_PER_FILE {
            c = c
                .chars()
                .take(PET_DROP_MAX_CHARS_PER_FILE)
                .collect::<String>();
            c.push_str("\n…(truncated)");
        }
        sections.push(format!("=== {fname} ===\n{c}"));
    }

    // Nothing readable (images/binaries): open chat with a reference note only.
    if sections.is_empty() {
        let names: Vec<String> = paths
            .iter()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.clone())
            })
            .collect();
        let draft = format!(
            "Dropped: {}\n\n(I can read text files for now — images need a vision model.)",
            names.join(", ")
        );
        let _ = app.emit(
            "pet-open-chat",
            serde_json::json!({ "agent": agent_name, "draft": draft }),
        );
        return Ok(PetDropResult {
            reply: String::new(),
            text_files: 0,
            skipped,
            remote_provider: None,
        });
    }

    let body = sections.join("\n\n");
    // Stage the full content in the Hub conversation for follow-up.
    let draft = format!("Here are the file(s) I dropped:\n\n{body}");
    let _ = app.emit(
        "pet-open-chat",
        serde_json::json!({ "agent": agent_name, "draft": draft }),
    );

    // Inline quick-take → returned to the pet as a bubble. Blocking dial runs off
    // the async worker so it never stalls the runtime (mirrors chat.rs).
    let prompt =
        format!("Give me a brief (1-2 sentence) take on the following dropped file(s):\n\n{body}");
    let agent = agent_name.clone();
    let join = tokio::task::spawn_blocking(move || {
        let home = mur_home();
        let task_id = format!(
            "pet-drop-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let params = serde_json::json!({
            "message": { "role": "user", "parts": [{ "kind": "text", "text": prompt }] },
            "task_id": task_id,
        });
        mur_core::a2a_dial::dial_method(
            &home,
            &agent,
            "message/send",
            params,
            mur_core::a2a_dial::DialMode::Auto,
        )
        .map(|task| extract_reply(&task))
        .map_err(|e| e.to_string())
    });
    // Bounded dial: a hung runtime can't leave the bubble spinning forever.
    let dialed = match tokio::time::timeout(std::time::Duration::from_secs(45), join).await {
        Ok(join_result) => join_result.map_err(|e| format!("pet drop task panicked: {e}"))?,
        Err(_) => Ok(format!("(timed out reaching {agent_name})")),
    };

    let reply = dialed.unwrap_or_else(|e| format!("(couldn't reach {agent_name}: {e})"));

    // Persist the exchange into the agent's channel so ChatTab re-hydrates and
    // shows the full take as a real panel message (best-effort, never blocks).
    {
        let agent_name2 = agent_name.clone();
        let file_label = paths
            .iter()
            .filter_map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .collect::<Vec<_>>()
            .join(", ");
        // Include any skipped names so the channel record is honest.
        let skipped_suffix = if skipped.is_empty() {
            String::new()
        } else {
            format!(" (skipped: {})", skipped.join(", "))
        };
        let user_msg = format!("📎 {file_label}{skipped_suffix}");
        let agent_reply = reply.clone();
        tokio::task::spawn_blocking(move || {
            let home = mur_home();
            mur_core::mobile::persist_mobile_exchange(&home, &agent_name2, &user_msg, &agent_reply);
        });
    }

    let remote_provider = resolve_remote_provider(&agent_name);

    Ok(PetDropResult {
        reply,
        text_files: sections.len(),
        skipped,
        remote_provider,
    })
}

#[tauri::command]
pub fn pet_get_expression(agent_name: String, expression: String) -> String {
    // Both segments become path components; reject traversal so a malicious
    // IPC caller can't read arbitrary files (the bytes are returned to the UI).
    if !valid_agent(&agent_name)
        || expression.is_empty()
        || expression.contains('/')
        || expression.contains('\\')
        || expression.contains("..")
        || expression.contains('\0')
    {
        return String::new();
    }
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

/// The pet's visual style, family, and whether real AI-rendered art exists.
/// When `has_ai_art` is false the UI renders the built-in vector mascot.
#[derive(Debug, Clone, Serialize)]
pub struct PetAppearance {
    pub style_preset: String,
    /// chibi | pixel | live2d | polaroid
    pub family: String,
    pub has_ai_art: bool,
}

/// Resolve `agent_name`'s pet appearance: its style preset, family, and whether
/// real `.webp` art is on disk (vs. the built-in vector mascot). Reads
/// `profile.yaml` for the preset id and the expressions manifest for the mode.
#[tauri::command]
pub fn pet_get_appearance(agent_name: String) -> PetAppearance {
    use mur_common::hub::preset_manifest::{RenderMode, manifest_path};
    use mur_common::hub::style_preset::PresetFamily;

    // Default shown if anything is missing/unreadable: the built-in blob, vector.
    let mut out = PetAppearance {
        style_preset: "default-blob".to_string(),
        family: "chibi".to_string(),
        has_ai_art: false,
    };
    if !valid_agent(&agent_name) {
        return out;
    }
    let agent_dir = mur_home().join("agents").join(&agent_name);

    // Style preset id from the profile.
    if let Ok(yaml) = std::fs::read_to_string(agent_dir.join("profile.yaml"))
        && let Ok(profile) = serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml)
    {
        out.style_preset = profile.appearance.style_preset;
    }

    // Family from the resolved preset (built-in or user).
    let hub_dir = mur_home().join("hub");
    if let Ok(preset) = mur_common::hub::preset_loader::find_preset(&out.style_preset, &hub_dir) {
        out.family = match preset.family {
            PresetFamily::Chibi => "chibi",
            PresetFamily::Pixel => "pixel",
            PresetFamily::Live2d => "live2d",
            PresetFamily::Polaroid => "polaroid",
        }
        .to_string();
    }

    // Real art only when the manifest says `Ai` AND an idle frame is on disk.
    if let Ok(json) = std::fs::read_to_string(manifest_path(&agent_dir))
        && let Ok(manifest) =
            serde_json::from_str::<mur_common::hub::preset_manifest::PresetManifest>(&json)
    {
        out.has_ai_art = manifest.mode == RenderMode::Ai
            && agent_dir.join("expressions").join("idle.webp").exists();
    }

    out
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    /// Pet windows live outside the `dashboard`/`popover` capabilities; without
    /// their own capability the ACL silently denies `listen()`/`startDragging()`
    /// and the pet is inert. Guard the capability file so it can't regress.
    #[test]
    fn pet_capability_covers_pet_windows() {
        let text = include_str!("../../capabilities/pet.json");
        let cap: serde_json::Value = serde_json::from_str(text).unwrap();

        let windows: Vec<&str> = cap["windows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w.as_str())
            .collect();
        assert!(
            windows.contains(&"pet-*"),
            "capability must match pet-* windows"
        );

        let perms: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p.as_str())
            .collect();
        // Every plugin command PetApp.tsx calls; app-defined commands need no ACL.
        for perm in [
            "core:event:allow-listen",
            "core:event:allow-unlisten",
            "core:window:allow-outer-position",
            "core:window:allow-start-dragging",
            // hide/show back the Hide-1h menu item (ACL-gated, was a silent no-op).
            "core:window:allow-hide",
            "core:window:allow-show",
        ] {
            assert!(perms.contains(&perm), "pet windows need {perm}");
        }
    }
}
