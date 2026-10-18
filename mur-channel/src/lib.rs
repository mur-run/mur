//! Unified Channel store: event-sourced JSONL log (source of truth) + (later)
//! a rebuildable SQLite query index + a file-watcher. Shared by the CLI
//! (`mur-core`) and the Hub (`mur-hub-gui`).
pub mod anchor;
pub mod governance;
pub mod head;
pub mod head_io;
pub mod id;
pub mod index;
pub mod purpose;
pub mod repair_marker;
pub mod service;
pub mod sign;
pub mod store;
pub mod summary;
pub mod watch;

pub use id::{check_channel_id, is_safe_channel_id};
pub use service::ChannelService;
pub use store::ChannelStore;
pub use summary::{
    ConversationQuery, ConversationSummary, RunSummary, SearchHit, SearchResults, SearchScope,
};
