//! Unified Channel store: event-sourced JSONL log (source of truth) + (later)
//! a rebuildable SQLite query index + a file-watcher. Shared by the CLI
//! (`mur-core`) and the Hub (`mur-hub-gui`).
pub mod governance;
pub mod index;
pub mod purpose;
pub mod service;
pub mod sign;
pub mod store;
pub mod summary;
pub mod watch;

pub use service::ChannelService;
pub use store::ChannelStore;
pub use summary::{ConversationQuery, ConversationSummary, RunSummary};
