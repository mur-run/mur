//! Tauri command surface for the companion bridge.
//!
//! Commands accept the agent name (string) and resolve the inbox dir
//! via `mur_root() / agents / <name> / companion / inbox`. The
//! `_inner` helpers exist so unit tests don't need a Tauri runtime.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::ipc::Channel;
use tokio::sync::mpsc::{self, Sender};

use super::event::BridgeEvent;
use super::scanner::scan_pending;
use super::state::BridgeState;
use super::watcher::InboxWatcher;

pub(crate) fn agent_inbox(home: &Path, agent: &str) -> PathBuf {
    home.join("agents").join(agent).join("companion/inbox")
}

/// Inner helper — testable without Tauri.
pub fn companion_bridge_pending_inner(home: &Path, agent: &str) -> Result<Vec<BridgeEvent>> {
    scan_pending(&agent_inbox(home, agent))
}

#[tauri::command]
pub async fn companion_bridge_pending(agent: String) -> Result<Vec<BridgeEvent>, String> {
    let home = mur_core::paths::mur_root(None);
    companion_bridge_pending_inner(&home, &agent).map_err(|e| format!("{e:#}"))
}

fn register_watcher(state: &BridgeState, agent: &str, watcher: InboxWatcher) -> Result<(), String> {
    state
        .watchers
        .lock()
        .map_err(|e| format!("{e}"))?
        .insert(agent.to_string(), watcher);
    Ok(())
}

/// Inner helper — testable. The `mpsc::Sender<BridgeEvent>` lets
/// integration tests observe events without a Tauri runtime. The
/// production command uses `register_watcher` directly because it
/// also needs to spawn a Channel forwarder.
///
/// `#[allow(dead_code)]`: this is exercised by `tests/bridge_state.rs`
/// (a separate test crate) but the bin/lib targets see it as unused.
#[allow(dead_code)]
pub fn companion_bridge_subscribe_inner(
    home: &Path,
    agent: &str,
    state: Arc<BridgeState>,
    tx: Sender<BridgeEvent>,
) -> Result<()> {
    let watcher = InboxWatcher::start(agent_inbox(home, agent), tx)?;
    register_watcher(&state, agent, watcher).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn companion_bridge_subscribe(
    agent: String,
    on_event: Channel<BridgeEvent>,
    state: tauri::State<'_, BridgeState>,
) -> Result<(), String> {
    let home = mur_core::paths::mur_root(None);
    let (tx, mut rx) = mpsc::channel::<BridgeEvent>(32);
    let watcher =
        InboxWatcher::start(agent_inbox(&home, &agent), tx).map_err(|e| format!("{e:#}"))?;
    register_watcher(&state, &agent, watcher)?;
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

// ── Notification + badge ─────────────────────────────────────────────────────

/// Plain-Rust DTO returned from `notify_payload_for` so unit tests
/// don't need a Tauri runtime.
#[derive(Debug, PartialEq)]
pub struct NotifyPayload {
    pub title: String,
    pub body: String,
}

/// Build the OS-level notification payload from a BridgeEvent's
/// situation tag + body. Title = situation (so the user knows *why*
/// it's pinging without needing zh-TW context). Body is the message
/// text, truncated to 200 chars with an ellipsis if longer.
pub fn notify_payload_for(situation: &str, body: &str) -> NotifyPayload {
    let total_chars = body.chars().count();
    let body_out = if total_chars > 200 {
        let mut s: String = body.chars().take(199).collect();
        s.push('…');
        s
    } else {
        body.to_string()
    };
    NotifyPayload {
        title: situation.to_string(),
        body: body_out,
    }
}

/// Clamp the unread count to a value the macOS dock can render.
/// `0` => `None` => clear the badge.
pub fn sanitize_badge_count(n: u32) -> Option<u32> {
    if n == 0 {
        None
    } else if n > 99 {
        Some(99)
    } else {
        Some(n)
    }
}

#[tauri::command]
pub async fn notify_message(
    app: tauri::AppHandle,
    situation: String,
    body: String,
) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    let payload = notify_payload_for(&situation, &body);
    app.notification()
        .builder()
        .title(payload.title)
        .body(payload.body)
        .show()
        .map_err(|e| format!("{e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn set_unread_badge(app: tauri::AppHandle, count: u32) -> Result<(), String> {
    // In Tauri 2.10 `set_badge_count` lives on `Window` / `WebviewWindow`
    // (not `AppHandle`). On macOS the badge is per-app, so calling it on
    // any window updates the dock for the whole app — we just need *a*
    // window. We pick the first webview window deterministically by
    // sorted label. The platform plugin handles main-thread dispatch
    // for us (works around tauri #13905 macOS Sonoma+ flake).
    use tauri::Manager;
    let value = sanitize_badge_count(count).map(|v| v as i64);
    let mut windows: Vec<_> = app.webview_windows().into_iter().collect();
    windows.sort_by(|a, b| a.0.cmp(&b.0));
    let Some((_label, window)) = windows.into_iter().next() else {
        // No window yet — silently ignore. The next call after a
        // window comes up will succeed.
        return Ok(());
    };
    window.set_badge_count(value).map_err(|e| format!("{e}"))?;
    Ok(())
}

// ── Ack ──────────────────────────────────────────────────────────────────────

/// Inner helper — testable. Rewrites the trailing
/// `>>> response: <unset>` line to `>>> response: <signal>` atomically
/// (write to .tmp + rename). The runtime's outbox loop picks the new
/// value up next time it scans the inbox dir.
///
/// Atomicity: the .tmp + rename pattern guarantees that a reader
/// either sees the old file or the new file — never a partially-written
/// state. This matters because the watcher's parse_inbox_md runs on
/// every Modify event and would error out on a torn write.
///
/// `#[allow(dead_code)]`: this is exercised by `tests/bridge_ack.rs`
/// (a separate test crate) but the bin/lib targets see it as unused
/// (the production path goes through the `companion_ack` Tauri
/// command directly).
#[allow(dead_code)]
pub fn companion_ack_inner(home: &Path, agent: &str, msg_id: &str, signal: &str) -> Result<()> {
    if !matches!(signal, "good" | "bad" | "dismiss") {
        anyhow::bail!("unknown signal `{signal}` (must be good|bad|dismiss)");
    }
    let path = agent_inbox(home, agent).join(format!("{msg_id}.md"));
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("read {} (msg_id={msg_id})", path.display()))?;
    let marker = ">>> response:";
    let (head, _) = body
        .rsplit_once(marker)
        .ok_or_else(|| anyhow::anyhow!("missing response marker in {}", path.display()))?;
    let new = format!("{head}{marker} {signal}");
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, new).context("write .tmp")?;
    std::fs::rename(&tmp, &path).context("rename .tmp -> .md")?;
    Ok(())
}

#[tauri::command]
pub async fn companion_ack(agent: String, msg_id: String, signal: String) -> Result<(), String> {
    let home = mur_core::paths::mur_root(None);
    companion_ack_inner(&home, &agent, &msg_id, &signal).map_err(|e| format!("{e:#}"))
}

// ── Why-did-you-message ──────────────────────────────────────────────────────

/// Inner helper — testable. Reads `<home>/agents/<agent>/companion/outbox-ledger`
/// (a JSONL file) and returns every line whose JSON contains
/// `"id":"<msg_id>"`. Order: append order (== chronological).
///
/// We intentionally do a substring match (not full JSON parse + filter)
/// because the ledger uses `#[serde(tag = "event")]` adjacent encoding —
/// every event variant carries the `id` at top level. A future schema
/// change would need to revisit this. The cheap-substring path keeps
/// the GUI from depending on the runtime's `OutboxEvent` enum and
/// tolerates ledger-format extensions gracefully.
///
/// `#[allow(dead_code)]`: this is exercised by `tests/bridge_why.rs`
/// (a separate test crate) but the bin/lib targets see it as unused
/// (the production path goes through the `companion_why` Tauri command).
#[allow(dead_code)]
pub fn companion_why_inner(
    home: &Path,
    agent: &str,
    msg_id: &str,
) -> Result<Vec<serde_json::Value>> {
    let ledger = home
        .join("agents")
        .join(agent)
        .join("companion/outbox-ledger");
    if !ledger.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let body =
        std::fs::read_to_string(&ledger).with_context(|| format!("read {}", ledger.display()))?;
    let needle = format!("\"id\":\"{msg_id}\"");
    for line in body.lines() {
        if line.contains(&needle)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
        {
            out.push(v);
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn companion_why(
    agent: String,
    msg_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let home = mur_core::paths::mur_root(None);
    companion_why_inner(&home, &agent, &msg_id).map_err(|e| format!("{e:#}"))
}

// ── Quiet / proactive toggles ────────────────────────────────────────────────

#[tauri::command]
pub async fn companion_proactive(agent: String, enabled: bool) -> Result<(), String> {
    mur_core::cmd::agent_companion::proactive::set_enabled(&agent, enabled)
        .map_err(|e| format!("{e:#}"))
}

/// Quiet-hours toggle. The GUI passes `for_seconds` (a literal seconds
/// count); the underlying CLI helper accepts the `<n>{s,m,h,d}` shorthand,
/// so we stringify as `<N>s`.
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
