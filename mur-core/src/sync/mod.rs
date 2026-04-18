//! Cross-process sync protocol client (mur CLI side).
//!
//! Outbox: persists [`mur_common::Signal`] YAML to `~/.mur/outbox/`.
//! Inbox: reads Signal YAML from `~/.mur/inbox/` and applies Evidence updates.
//! See also: Task 10 SyncClient.

pub mod inbox;
pub mod outbox;

pub use inbox::{ApplyReport, Inbox};
pub use outbox::Outbox;
