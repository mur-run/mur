//! `B0SafetyHook` — stub for M0.
//!
//! The 22 baseline rules from roadmap §6.1 (12 text + 10 multimodal)
//! land in B0's own milestone (M8 in roadmap §7.3). M0 only locks the
//! slot in the chain so later milestones add behavior without
//! touching call sites.
//!
//! When implemented (M8), this hook will own:
//! * `on_prompt_submit` — untrusted wrappers, secret pre-filter
//! * `pre_tool_use` — no-chain-after-untrusted, AskUser triggers,
//!   GrantStore lookup
//! * `post_tool_use` — redaction
//! * `on_message_received` — set untrusted flag based on envelope
//!   metadata
//! * `on_startup` — sandbox attestation

use crate::hooks::Hook;

pub struct B0SafetyHook;

impl B0SafetyHook {
    pub fn new() -> Self {
        Self
    }
}

impl Default for B0SafetyHook {
    fn default() -> Self {
        Self::new()
    }
}

impl Hook for B0SafetyHook {
    fn name(&self) -> &str {
        "B0SafetyHook"
    }
}
