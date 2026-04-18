//! Cross-process sync protocol client (mur CLI side).
//!
//! Outbox: persists [`mur_common::Signal`] YAML to `~/.mur/outbox/`.
//! See also: Task 8 Inbox, Task 10 SyncClient.

pub mod outbox;

pub use outbox::Outbox;
