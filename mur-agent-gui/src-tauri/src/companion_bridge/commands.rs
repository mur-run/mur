//! Tauri command surface for the companion bridge.
//!
//! Commands accept the agent name (string) and resolve the inbox dir
//! via `mur_root() / agents / <name> / companion / inbox`. The
//! `_inner` helpers exist so unit tests don't need a Tauri runtime.

use anyhow::Result;
use std::path::{Path, PathBuf};

use super::event::BridgeEvent;
use super::scanner::scan_pending;

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
