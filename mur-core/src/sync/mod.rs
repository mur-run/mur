//! Cross-process sync protocol client (mur CLI side).
//!
//! - [`Outbox`]: persists [`mur_common::Signal`] YAML to `~/.mur/outbox/`.
//! - [`Inbox`]: reads Signal YAML from `~/.mur/inbox/` and applies Evidence updates.
//! - [`CursorStore`]: persistent fetch cursor for server pagination.
//!
//! See also: Task 10 SyncClient (HTTP layer).

pub mod cursor;
pub mod inbox;
pub mod outbox;

pub use cursor::{CursorStore, FetchCursor};
pub use inbox::{ApplyReport, Inbox};
pub use outbox::Outbox;
