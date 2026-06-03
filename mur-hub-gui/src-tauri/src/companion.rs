//! Companion bridge Tauri commands for mur-hub-gui.
//!
//! Wraps `mur_gui_core::companion_bridge` with the Tauri command surface.
//! Manages one `InboxWatcher` per subscribed agent via a `BridgeState`
//! managed state value.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use mur_gui_core::companion_bridge::{BridgeEvent, BridgeResponse, InboxWatcher, scan_pending};
use tauri::ipc::Channel;
use tokio::sync::mpsc;

// ─── Managed state ───────────────────────────────────────────────────────────

pub struct BridgeState {
    pub watchers: Mutex<HashMap<String, InboxWatcher>>,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self {
            watchers: Mutex::new(HashMap::new()),
        }
    }
}

// ─── Path helpers ─────────────────────────────────────────────────────────────

fn mur_home() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".mur"))
}

fn agent_inbox(agent: &str) -> PathBuf {
    mur_home()
        .join("agents")
        .join(agent)
        .join("companion/inbox")
}

/// Reject agent names that aren't safe as a path component before they're
/// joined under `~/.mur/agents/`. Uses the canonical agent-name rules so the
/// GUI and CLI/HTTP surfaces agree.
fn check_agent(agent: &str) -> Result<(), String> {
    mur_common::agent_name::validate_agent_name(agent).map_err(|e| format!("invalid agent name: {e}"))
}

/// Lightweight traversal check for an opaque id used as a filename component
/// (lenient on charset so real message ids still pass, strict on separators).
fn check_path_component(s: &str, what: &str) -> Result<(), String> {
    if s.is_empty() || s.contains('/') || s.contains('\\') || s.contains("..") || s.contains('\0') {
        return Err(format!("invalid {what}"));
    }
    Ok(())
}

// ─── Tauri commands ──────────────────────────────────────────────────────────

/// Return all pending (unresponded) inbox messages for `agent`.
#[tauri::command]
pub fn companion_bridge_pending(agent: String) -> Result<Vec<BridgeEvent>, String> {
    check_agent(&agent)?;
    scan_pending(&agent_inbox(&agent)).map_err(|e| format!("{e:#}"))
}

/// Subscribe to new inbox messages via a Tauri IPC `Channel`.
/// The watcher is kept alive in `BridgeState` until `companion_bridge_unsubscribe` is called.
#[tauri::command]
pub async fn companion_bridge_subscribe(
    agent: String,
    on_event: Channel<BridgeEvent>,
    state: tauri::State<'_, BridgeState>,
) -> Result<(), String> {
    check_agent(&agent)?;
    let (tx, mut rx) = mpsc::channel::<BridgeEvent>(32);
    let watcher = InboxWatcher::start(agent_inbox(&agent), tx).map_err(|e| format!("{e:#}"))?;

    state
        .watchers
        .lock()
        .map_err(|e| format!("{e}"))?
        .insert(agent.clone(), watcher);

    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Err(e) = on_event.send(ev) {
                tracing::warn!("companion_bridge: channel send failed: {e}");
                break;
            }
        }
    });
    Ok(())
}

/// Remove the watcher for `agent` (stops forwarding new events).
#[tauri::command]
pub fn companion_bridge_unsubscribe(
    agent: String,
    state: tauri::State<'_, BridgeState>,
) -> Result<(), String> {
    check_agent(&agent)?;
    state
        .watchers
        .lock()
        .map_err(|e| format!("{e}"))?
        .remove(&agent);
    Ok(())
}

/// Acknowledge a message (rewrite `>>> response: <unset>` → `>>> response: <signal>`).
#[tauri::command]
pub fn companion_ack(agent: String, msg_id: String, signal: String) -> Result<(), String> {
    check_agent(&agent)?;
    check_path_component(&msg_id, "msg_id")?;
    if !matches!(signal.as_str(), "good" | "bad" | "dismiss" | "snooze") {
        return Err(format!("unknown signal `{signal}`"));
    }
    let path = agent_inbox(&agent).join(format!("{msg_id}.md"));
    let body =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let marker = ">>> response:";
    let Some((head, _)) = body.rsplit_once(marker) else {
        return Err(format!("missing response marker in {}", path.display()));
    };
    let new = format!("{head}{marker} {signal}");
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, new).map_err(|e| format!("write .tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename .tmp: {e}"))?;
    Ok(())
}

/// Count unread (BridgeResponse::Unset) messages for `agent`.
#[tauri::command]
pub fn companion_unread_count(agent: String) -> u32 {
    if check_agent(&agent).is_err() {
        return 0;
    }
    scan_pending(&agent_inbox(&agent))
        .map(|evs| {
            evs.iter()
                .filter(|e| e.response == BridgeResponse::Unset)
                .count() as u32
        })
        .unwrap_or(0)
}

/// Toggle proactive mode for `agent`.
#[tauri::command]
pub fn companion_proactive(agent: String, enabled: bool) -> Result<(), String> {
    mur_core::cmd::agent_companion::proactive::set_enabled(&agent, enabled)
        .map_err(|e| format!("{e:#}"))
}

/// Set/clear quiet hours for `agent`.
#[tauri::command]
pub async fn companion_quiet(
    agent: String,
    for_seconds: Option<i64>,
    until: Option<String>,
    off: bool,
) -> Result<(), String> {
    let for_str = for_seconds.map(|n| format!("{n}s"));
    mur_core::cmd::agent_companion::quiet::set(&agent, for_str.as_deref(), until.as_deref(), off)
        .await
        .map_err(|e| format!("{e:#}"))
}
