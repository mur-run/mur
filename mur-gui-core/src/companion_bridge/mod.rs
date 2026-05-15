//! Companion → GUI IPC bridge (D5).
//!
//! Pure-Rust core shared between mur-hub-gui and (during the migration period)
//! mur-agent-gui.  No Tauri, no mur-core, no mur-agent-runtime dependencies
//! here — those live in the per-app command layers.
//!
//! Layout:
//!   `event`   — BridgeEvent type + inbox markdown parser
//!   `scanner` — one-shot scan of an inbox directory
//!   `watcher` — filesystem-event watcher → BridgeEvent on a tokio mpsc

pub mod event;
pub mod scanner;
pub mod watcher;

pub use event::{BridgeEvent, BridgeResponse};
pub use scanner::scan_pending;
pub use watcher::InboxWatcher;
