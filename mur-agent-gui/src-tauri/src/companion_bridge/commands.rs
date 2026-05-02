//! Tauri command surface for the companion bridge.
//!
//! Commands accept the agent name (string) and resolve the inbox dir
//! via `mur_root() / agents / <name> / companion / inbox`. The
//! `_inner` helpers exist so unit tests don't need a Tauri runtime.

use anyhow::Result;
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
