//! Cross-process sync protocol client (mur CLI side).
//!
//! - [`Outbox`]: persists [`mur_common::Signal`] YAML to `~/.mur/outbox/`.
//! - [`Inbox`]: reads Signal YAML from `~/.mur/inbox/` and applies Evidence updates.
//! - [`CursorStore`]: persistent fetch cursor for server pagination.
//!
//! See also: Task 10 SyncClient (HTTP layer).

pub mod client;
pub mod cursor;
pub mod inbox;
pub mod outbox;

#[allow(unused_imports)]
pub use client::{DraftRecord, DraftsPendingResponse, SyncClient};
pub use cursor::{CursorStore, FetchCursor};
pub use inbox::Inbox;
pub use outbox::Outbox;
