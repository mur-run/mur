//! Unified Channel store: event-sourced JSONL log (source of truth) + (later)
//! a rebuildable SQLite query index + a file-watcher. Shared by the CLI
//! (`mur-core`) and the Hub (`mur-hub-gui`).
pub mod store;
pub mod index;
pub mod service;
pub mod watch;

pub use store::ChannelStore;
pub use service::ChannelService;
