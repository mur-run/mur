//! `LedgerHook` — stub for M0.
//!
//! Originally intended to append an `OutboxEvent::MessageSent` to the
//! companion durable ledger on every `on_message_send`. That would
//! conflate proactive companion sends with reactive replies and corrupt
//! the frozen schema (R12 invariant from companion phase 1.1).
//!
//! The hook slot is reserved here so the chain registration order is
//! stable; the real wiring lands when reactive-reply ledger semantics
//! are designed (B0 / M8 or a follow-up companion phase).

use crate::hooks::Hook;

pub struct LedgerHook;

impl LedgerHook {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LedgerHook {
    fn default() -> Self {
        Self::new()
    }
}

impl Hook for LedgerHook {
    fn name(&self) -> &str {
        "LedgerHook"
    }
}
