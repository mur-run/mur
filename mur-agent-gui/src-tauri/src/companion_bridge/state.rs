//! Per-process bridge state.
//!
//! Holds one `InboxWatcher` per subscribed agent. Drop the entry to
//! tear down the watcher.

use std::collections::HashMap;
use std::sync::Mutex;

use super::watcher::InboxWatcher;

#[derive(Default)]
pub struct BridgeState {
    pub watchers: Mutex<HashMap<String, InboxWatcher>>,
}
